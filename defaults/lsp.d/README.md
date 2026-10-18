# defaults/lsp.d

One file per language server. A server is used only when its program is already on
`PATH`; quack never installs one. Each file's comment says how to install it.

Servers start the first time the agent reads or edits a file they cover. After an edit,
new errors and warnings are added to the edit's result, and the `lsp` tool gives the
model definitions, references, hover text, symbols and diagnostics.

The smallest file:

```toml
command = "gopls"
extensions = ["go"]
```

Delete a file, or list its id in `[lsp] disabled = ["…"]` in config.toml, to turn a server
off. See [../../docs/plan/10-lsp.md](../../docs/plan/10-lsp.md).
