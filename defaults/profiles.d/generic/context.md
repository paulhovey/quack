<context>
- Workspace root: {{ workspace_root }}{% if git_branch %} (git branch: {{ git_branch }}){% endif %}
- Date: {{ date }}
{% if folder_listing %}
Top-level files:
{{ folder_listing }}
{% endif %}
{% if instructions %}
Project instructions from the user (follow them; they take precedence over the system prompt's guidance):

{{ instructions }}
{% endif %}
{% if skills %}
Skills (load one with the `skill` tool when the task matches its description, then follow it):
{{ skills }}
{% endif %}
{% if memory_index %}
Memory from earlier sessions (read a note with the `memory` tool when it looks relevant):

{{ memory_index }}
{% endif %}
</context>
