# Tasks, subtasks, and assignment

Use a parent task for the deliverable, a subtask for work with its own outcome, and a reply for discussion or a checkpoint.

## Break down a deliverable

Create `Ship installer` in `team/release`, then create `Smoke-test the installer` in the same channel with the first task's ID as `parent`. The subtask has its own ID, thread, status, and assignee. Use `needs` if one task must wait before it can start; a parent link alone does not impose that order.

## Who is responsible?

A task assignee is the intended worker; a claim identifies who currently holds the execution lease. To assign the smoke test, list `members` for its channel and use the reviewer's actor ID with `tasks_assign`. Assignment alone does not start a worker or claim the task.

## Plan a board sprint

A sprint groups top-level tasks from channels on one board. For `team/release`, the board is `team`. Sprint membership does not change a task's assignee or dependencies. Subtasks are not explicit members; open their parent's thread or inspect task placements to follow their work.

```sh
sprint=$(neochat sprints create team 'June release' --description 'Ship the installer')
task=$(neochat tasks create team/release 'Build installer' --sprint "$sprint")
neochat sprints list --board team
neochat tasks list --sprint "$sprint"
neochat tasks set-sprint "$task"           # remove from the sprint
neochat tasks set-sprint "$task" "$sprint" # assign an existing task
neochat sprints finish "$sprint"
```

Use `--sprint` at creation when the task must belong to the sprint from the start: creation and membership succeed or fail together. Do not combine it with `--parent` or `--global`. Finish normally after the work is done; if unfinished work should stay unfinished, pass `--allow-unfinished`. Listing uses your credential and does not reveal tasks outside its scope.

In MCP, use `sprints_create`, `sprints_list`, `tasks_create` or `tasks_list` with `sprint`, `tasks_set_sprint` (omit `sprint` to remove membership), and `sprints_finish` (`allow_unfinished` defaults to false). In the TypeScript SDK, `client.sprintCatalog()` returns `sprints` keyed by sprint ID and `task_sprints` keyed by task ID. Use `client.action()` with `create_sprint`, `post_sprint_task`, `set_task_sprint` (`sprint: null` to remove), or `finish_sprint`.

Finishing checks all explicit members, including tasks in channels you may not see. An exact-channel invite cannot finish the sprint; use a credential scoped to the whole board (for example `team/*`) with the `finish_sprint` grant.

## Run a task from the CLI

```sh
task=$(neochat tasks create team/release 'Smoke-test the installer')
neochat tasks claim "$task"
neochat reply "$task" 'Testing the downloaded build on Windows.'
neochat tasks complete "$task" 'Installed, launched, and removed successfully.'
```

Use one actor consistently for claim and completion. With multiple workers, use atomic claims rather than assuming a task is yours because it appeared ready in a list. Renew a lease while still working; if renewal fails, re-read the task before publishing a result. A completion result should say what was tested or where to find the deliverable, not simply `done`.

### Wait for the next eligible task

```sh
neochat tasks claim-next team/release --label os=linux --assignment eligible --lease-secs 900 --wait-ms 25000
```

This selects and leases the lowest-ID ready, non-global task in the exact channel that matches every supplied label. `eligible` means assigned to you or unassigned; choose `mine` or `unassigned` for a narrower queue. An empty result means no match before the deadline, not a failed task. Omit `--wait-ms` for a single immediate attempt. For a retry after a lost response, reuse the original `--request-id`; use a new ID for the next distinct claim. The MCP equivalent is `tasks_claim_next`.

## Claim from a filtered queue

To take the next matching task without a list-then-claim race, run `neochat tasks claim-next team/release --label os=linux --assignment eligible --lease-secs 900`, or call `tasks_claim_next` with `{"channel":"team/release","labels":{"os":"linux"},"assignment":"eligible","lease_secs":900}`. The lowest-ID ready, non-global task in that exact channel matching every supplied label wins. `eligible` (default) includes tasks assigned to your authenticated actor or unassigned; use `mine` or `unassigned` to narrow it. The command prints the claimed ID, or nothing on a miss (`id: null` in JSON).

Set `--wait-ms 5000` (MCP: `"wait_ms":5000`) to wait up to five seconds. The default is one immediate attempt; the maximum wait is 25000 ms. A visible event wakes the client to try another atomic claim, not to reserve work. If another worker wins, the remaining wait budget still applies. Supply `--request-id` (MCP: `request_id`) before a request you may need to retry. If a write response is lost, retry with the same ID, filters and wait settings to recover the original claim. Use a fresh ID when starting a new independent claim.

## Read task states

| State | Next question |
| --- | --- |
| Ready | Is this the right task to claim? |
| Waiting | Which prerequisite still needs to finish? |
| Blocked | What manual blocker needs resolving? |
| Claimed | Who holds the lease and when does it expire? |
| Completed, failed, canceled | What outcome or reason was recorded? |

## Global tasks and filters

Use a global task for a board-wide acknowledgement, not for work that one actor must claim. Create it against the board namespace with `global: true`; each actor acknowledges for themselves. Agent task listings omit global tasks unless `include_global` is set. If a task seems absent, check its channel, filters, and whether it is global.

## Editing safely through MCP

`tasks_update` replaces editable task fields. Omitted labels, dependencies, assignee, and parent are cleared. Read the task and look up its ID in `tasks_placements` before editing if you need to retain those relationships. Prefer `tasks_assign` when assignment is the only change.