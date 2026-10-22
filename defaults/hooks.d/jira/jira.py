#!/usr/bin/env python3
"""A quack `user_prompt` hook: add the Jira issues a prompt links to.

quack runs this when the prompt matches the hook's `match.prompt` and writes the event
to stdin as JSON (`prompt`, and `matches`: the links it found). For each link to the
Jira at JIRA_URL, this fetches the issue over the REST API (Server, Data Center or
Cloud) and prints it as XML for the model, as `additional_context`.

Settings come from the environment, which the hook file sets with `env`:

  JIRA_URL           the Jira base URL, e.g. https://jira.example.com (required)
  JIRA_TOKEN         a personal access token (Server/Data Center), or an API token
                     (Cloud, together with JIRA_USER) (required)
  JIRA_USER          Cloud only: the account email; switches to basic authentication
  JIRA_MAX_ISSUES    issues fetched per prompt (default 5)
  JIRA_MAX_COMMENTS  newest comments kept per issue (default 5)
  JIRA_MAX_CHARS     longest description or comment kept, in characters (default 4000)
  JIRA_CA_BUNDLE     a PEM file of CA certificates (e.g. a corporate intermediate) added
                     to the system trust store, never replacing it. Pass the server's
                     intermediate here when its issuer root is already trusted by the system.
  JIRA_TIMEOUT       seconds per request (default 8)

Only the Python standard library is used. Exit status 1 with a message on stderr means
the hook is misconfigured; quack shows it as a warning and sends the prompt anyway.
"""

import base64
import json
import os
import re
import ssl
import sys
import urllib.error
import urllib.parse
import urllib.request
from xml.sax.saxutils import escape, quoteattr

FIELDS = ",".join(
    [
        "summary",
        "issuetype",
        "status",
        "priority",
        "resolution",
        "assignee",
        "reporter",
        "created",
        "updated",
        "labels",
        "components",
        "fixVersions",
        "description",
        "comment",
        "issuelinks",
        "subtasks",
        "parent",
    ]
)

KEY = re.compile(r"/browse/([A-Z][A-Z0-9_]+-\d+)")


def setting(name, default=None):
    value = os.environ.get(name, "").strip()
    return value or default


def number(name, default):
    try:
        return max(0, int(setting(name, str(default))))
    except ValueError:
        return default


def fail(message):
    print(message, file=sys.stderr)
    sys.exit(1)


def issue_keys(event, base):
    """Issue keys from links to `base`, in prompt order, without repeats."""
    links = event.get("matches") or re.findall(r"https?://\S+", event.get("prompt", ""))
    host = urllib.parse.urlsplit(base).netloc.lower()
    keys = []
    for link in links:
        if urllib.parse.urlsplit(link).netloc.lower() != host:
            continue
        m = KEY.search(link)
        if m and m.group(1) not in keys:
            keys.append(m.group(1))
    return keys


def opener():
    # Trust the system root store (on a corporate workstation this includes any
    # TLS-intercepting root enrolled there, e.g. GlobalSign Root CA - R3), then load
    # JIRA_CA_BUNDLE *on top of* it, never instead of it. Passing `cafile=` to
    # create_default_context() replaces the system store, so an intermediate such
    # as GlobalSign RSA OV SSL CA 2018 ends up trusted on its own and can no longer
    # reach its issuer root -> "unable to get issuer certificate".
    context = ssl.create_default_context()
    bundle = setting("JIRA_CA_BUNDLE")
    if bundle:
        path = os.path.expanduser(bundle)
        if os.path.exists(path):
            context.load_verify_locations(path)
        else:
            fail(f"JIRA_CA_BUNDLE is {path}, which does not exist; remove it or fix the path")
    # urllib honours HTTPS_PROXY and NO_PROXY, which quack passes through.
    return urllib.request.build_opener(urllib.request.HTTPSHandler(context=context))


def authorization():
    token = setting("JIRA_TOKEN") or fail(
        "JIRA_TOKEN is not set; set it in the hook file's [env] (see hooks.d/jira.toml)"
    )
    user = setting("JIRA_USER")
    if user:
        pair = base64.b64encode(f"{user}:{token}".encode()).decode()
        return f"Basic {pair}"
    return f"Bearer {token}"


def fetch(http, base, auth, key):
    url = f"{base}/rest/api/2/issue/{urllib.parse.quote(key)}?fields={FIELDS}"
    request = urllib.request.Request(
        url, headers={"Authorization": auth, "Accept": "application/json"}
    )
    with http.open(request, timeout=number("JIRA_TIMEOUT", 8)) as response:
        return json.load(response)


def text(value, limit):
    value = (value or "").strip()
    if len(value) > limit:
        value = value[:limit].rstrip() + f"\n[… cut at {limit} characters]"
    return escape(value)


def name(person):
    return (person or {}).get("displayName") or ""


def names(items):
    return ", ".join(i.get("name", "") for i in items or [] if i.get("name"))


def render(base, key, issue):
    f = issue.get("fields") or {}
    limit = number("JIRA_MAX_CHARS", 4000)
    out = [f"<jira_issue key={quoteattr(key)} url={quoteattr(f'{base}/browse/{key}')}>"]

    def tag(name_, value):
        if value:
            out.append(f"  <{name_}>{escape(str(value))}</{name_}>")

    tag("summary", f.get("summary"))
    tag("type", (f.get("issuetype") or {}).get("name"))
    tag("status", (f.get("status") or {}).get("name"))
    tag("priority", (f.get("priority") or {}).get("name"))
    tag("resolution", (f.get("resolution") or {}).get("name"))
    tag("assignee", name(f.get("assignee")))
    tag("reporter", name(f.get("reporter")))
    tag("created", f.get("created"))
    tag("updated", f.get("updated"))
    tag("labels", ", ".join(f.get("labels") or []))
    tag("components", names(f.get("components")))
    tag("fix_versions", names(f.get("fixVersions")))
    parent = f.get("parent")
    if parent:
        summary = (parent.get("fields") or {}).get("summary", "")
        out.append(f"  <parent key={quoteattr(parent.get('key', ''))}>{escape(summary)}</parent>")
    if f.get("description"):
        out.append(f"  <description>\n{text(f.get('description'), limit)}\n  </description>")

    links = []
    for link in f.get("issuelinks") or []:
        kind = link.get("type") or {}
        if "outwardIssue" in link:
            other, relation = link["outwardIssue"], kind.get("outward", "relates to")
        elif "inwardIssue" in link:
            other, relation = link["inwardIssue"], kind.get("inward", "relates to")
        else:
            continue
        of = other.get("fields") or {}
        status = (of.get("status") or {}).get("name", "")
        links.append(
            f"    <link relation={quoteattr(relation)} key={quoteattr(other.get('key', ''))}"
            f" status={quoteattr(status)}>{escape(of.get('summary', ''))}</link>"
        )
    for sub in f.get("subtasks") or []:
        sf = sub.get("fields") or {}
        status = (sf.get("status") or {}).get("name", "")
        links.append(
            f"    <link relation=\"subtask\" key={quoteattr(sub.get('key', ''))}"
            f" status={quoteattr(status)}>{escape(sf.get('summary', ''))}</link>"
        )
    if links:
        out += ["  <links>", *links, "  </links>"]

    comments = (f.get("comment") or {}).get("comments") or []
    keep = number("JIRA_MAX_COMMENTS", 5)
    if comments and keep:
        shown = comments[-keep:]
        out.append(f'  <comments shown="{len(shown)}" total="{len(comments)}">')
        for c in shown:
            out.append(
                f"    <comment author={quoteattr(name(c.get('author')))}"
                f" created={quoteattr(c.get('created', ''))}>\n"
                f"{text(c.get('body'), limit)}\n    </comment>"
            )
        out.append("  </comments>")
    out.append("</jira_issue>")
    return "\n".join(out)


def unavailable(key, why):
    return f"<jira_issue key={quoteattr(key)} unavailable={quoteattr(why)} />"


def main():
    event = json.load(sys.stdin)
    base = (setting("JIRA_URL") or fail(
        "JIRA_URL is not set; set it in the hook file's [env] (see hooks.d/jira.toml)"
    )).rstrip("/")
    keys = issue_keys(event, base)[: number("JIRA_MAX_ISSUES", 5)]
    if not keys:
        return  # links to another Jira: nothing to add
    auth = authorization()
    http = opener()
    blocks, fetched = [], []
    for key in keys:
        try:
            blocks.append(render(base, key, fetch(http, base, auth, key)))
            fetched.append(key)
        except urllib.error.HTTPError as e:
            if e.code == 401:
                fail(f"Jira at {base} rejected the token (HTTP 401); check JIRA_TOKEN")
            why = "not found, or no permission to see it" if e.code == 404 else f"HTTP {e.code}"
            blocks.append(unavailable(key, why))
        except (urllib.error.URLError, TimeoutError, OSError) as e:
            reason = getattr(e, "reason", e)
            fail(f"cannot reach Jira at {base}: {reason}")
        except ValueError:
            blocks.append(unavailable(key, "Jira answered with something that is not JSON"))
    missing = [k for k in keys if k not in fetched]
    message = f"fetched {', '.join(fetched)}" if fetched else "no issues fetched"
    if missing:
        message += f"; could not fetch {', '.join(missing)}"
    print(json.dumps({
        "additional_context": "Jira issues linked in this message:\n" + "\n".join(blocks),
        "message": message,
    }))


if __name__ == "__main__":
    main()
