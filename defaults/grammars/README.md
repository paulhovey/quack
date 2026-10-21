# defaults/grammars

Query overrides for the tree-sitter grammars compiled into quack. The built-in queries are
compiled in, so this folder ships nothing but this README. The same layout works in the
user config dir (`grammars/`) and in a trusted project's `.quack/grammars/`.

```
grammars/
  <lang>/            rust, python, javascript, typescript, tsx, go, c, cpp or bash
    tags.scm         definitions for `index`, outlined reads and `read symbol=`
    highlights.scm   colours for code blocks in the TUI
    injections.scm   languages embedded in others
    locals.scm       scopes, for highlighting
```

A file replaces the query from the layers below it. When its first line is `; extends`, it
is added after that query instead. For example, a project whose Rust code declares routes
with a `route!` macro can have them show in outlines:

```scheme
; extends
(macro_invocation
  macro: (identifier) @m (#eq? @m "route")
  (token_tree (identifier) @name)) @definition.route
```

`tags.scm` captures a definition as `@definition.<kind>` and its name as `@name`.
`highlights.scm` uses the usual names (`@keyword`, `@string`, `@function.method`, …).

A query that does not compile is logged, the built-in one is used instead, and
`quack doctor` shows the error. Folders for other languages are ignored: new grammars
can't be loaded at runtime yet.
