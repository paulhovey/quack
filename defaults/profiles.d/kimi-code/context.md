<context>
- Workspace: {{ workspace_root }}{% if git_branch %} (git branch: {{ git_branch }}){% endif %} (only worry about this folder and its sub-folders; do not inspect or modify files outside it)
- Date: {{ date }}
{% if folder_listing %}
Workspace top level:
{{ folder_listing }}
{% endif %}
{% if instructions %}
## Project instructions
From the user; they take precedence over the system prompt's guidance.

{{ instructions }}
{% endif %}
{% if skills %}
## Skills
Load a skill with the `skill` tool when the task matches it.
{{ skills }}
{% endif %}
{% if memory_index %}
## Memory
{{ memory_index }}
{% endif %}
</context>
