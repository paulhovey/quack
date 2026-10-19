from report.format import render


def build(source):
    rows = [source]
    render(rows, "report.txt")
