<context>
- Workspace root: {{ workspace_root }}{% if git_branch %} (git branch: {{ git_branch }}){% endif %} (only worry about this folder and its sub-folders; do not inspect or modify files outside it)
- Today's date: {{ date }}
{% if folder_listing %}
Top-level contents of the workspace:
{{ folder_listing }}
{% endif %}
{% if instructions %}
# Project instructions
These come from the user and take precedence over the system prompt's guidance.

{{ instructions }}
{% endif %}
{% if skills %}
# Skills
Load one with the `skill` tool when the task matches its description, then follow it.
{{ skills }}
{% endif %}
{% if memory_index %}
# Memory
Notes from earlier sessions; read one when it looks relevant.

{{ memory_index }}
{% endif %}
</context>
