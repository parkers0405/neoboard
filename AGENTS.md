# NeoBoard task updates

Use the board MCP adapter for agent-authored updates so the sender is recorded. For CLI fallback, pass `--agent-framework Neoism` (substitute the actual framework) or set `NEOCHAT_AGENT_FRAMEWORK` in that agent's environment. Do not silently post as a person. Write a short single-line task title. Put scope, instructions and acceptance criteria in the MCP `description` or CLI `--description` instead of the list title.

For work tracked on NeoBoard, the board is the source of truth. Update the task when the work changes, not just in the final chat response. Use the assigned board and task ID; for this project's roadmap, use `NeoBoard/roadmap`. A read-only question does not require creating or claiming a task.

| Checkpoint | Required board update |
| --- | --- |
| Start | Find the existing task before creating another. Read its thread and acceptance criteria. Claim it before working; if someone else holds the lease, coordinate instead of taking it over. |
| Delegate | Give each subagent the board, task or subtask ID, scope, acceptance criteria, and who will post updates. Use a subtask for independently verifiable work. Only one actor holds the lease on any given task. |
| Make progress | Reply in the task thread after each meaningful milestone: what changed, what evidence exists, and what remains. Renew the lease while actively working. Do not post a heartbeat for every command. |
| Hit a blocker or hand off | Record the obstacle, its impact, and the specific decision or action needed. Set the appropriate task state. For a handoff, name the next owner and the next step; do not mark partial work complete. |
| Verify and finish | Check every acceptance criterion. Complete the task with the checks run, their results, and any limits. Distinguish verified locally, pushed, and released. If a criterion is unmet, leave the task open and say which one. |
| Report back | Re-read the task to verify the update landed before reporting its status to the user. A subagent's completion notification is evidence to review, not a board update. |

Use your own authorized actor where available. If a subagent has no board access or was told not to touch the live board, it must send each checkpoint to its parent. The parent posts it promptly on the same task, identifies whose work it is reporting, and verifies the resulting state. Never share a host token or invent a separate actor to make an update appear agent-authored. If neither agent can write, report the unsynced task ID and checkpoint; reconcile when access returns rather than claiming the board is current.