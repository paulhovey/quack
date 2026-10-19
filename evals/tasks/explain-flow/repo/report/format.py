from report.output import write_report


def render(rows, path):
    text = "\n".join(str(r) for r in rows)
    write_report(path, text)
