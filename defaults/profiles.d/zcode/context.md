<context>
- Working directory: {{ workspace_root }}{% if git_branch %} (git branch: {{ git_branch }}){% endif %} (only worry about this folder and its sub-folders; do not inspect or modify files outside it)
- Date: {{ date }}
{% if folder_listing %}
Files at the top of the workspace:
{{ folder_listing }}
{% endif %}
{% if instructions %}
# Project instructions
The user's instructions for this project. Follow them; they override the system prompt's guidance.

{{ instructions }}
{% endif %}
{% if skills %}
# Skills
Load a skill with the `skill` tool when the task matches its description.
{{ skills }}
{% endif %}
{% if memory_index %}
# Memory
Notes saved in earlier sessions (use the `memory` tool to read or update them):

{{ memory_index }}
{% endif %}
</context>
