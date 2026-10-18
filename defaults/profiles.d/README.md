# defaults/profiles.d

Model profiles, one directory each: `profile.toml` plus a `system.md` prompt template
(minijinja). Delete a directory to remove that profile; with none installed the binary
uses its compiled-in empty profile.

- `generic`: a short original prompt for any model, using the internal tool names.

More profiles shaped after each model family's own harness land in M2
(docs/plan/03-model-profiles.md).
