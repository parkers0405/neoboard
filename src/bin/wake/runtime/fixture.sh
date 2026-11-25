#!/bin/sh
# Fake CLI; only reads/writes its per-test temp workspace. No model or provider.
set -eu
test -z "${NEOCHAT_TOKEN+x}${NEOBOARD_INBOX_TOKEN+x}${NEOISM_WAKE_TOKEN+x}${FAKE_OTHER_BOARD_CREDENTIAL+x}"
mode=ok
if test -f mode; then mode=$(cat mode); fi
if test "$mode" = hang; then sleep 30; fi
sid=00000000-0000-4000-8000-000000000017
reply_id=$sid
if test "$mode" = wrong; then reply_id=other; fi
if test "$1" = --version; then test "$#" = 1; echo fixture; exit; fi
if test "$1" = queue; then
    test -f queue || exit 2 # an older Codex: unknown subcommand, nothing queued
    test "$#" = 6 && test "$2" = --thread && test "$3" = "$sid" && test "$4" = --message && test "$6" = --resume || exit 3
    case "$(cat queue)" in
        ok) printf %s "$5" >queued; echo "Queued message fixture for thread $3.";;
        silent) exit 0;;
        *) exit 1;;
    esac
    exit
fi
if test "$1" = mcp; then printf '%s\n' "$@" >>configured; test ! -f mcp-fail; exit; fi
if test "$1" = --resume; then
    test "$#" -ge 7 && test "$2" = "$sid" && test "$3" = --print && test "$4" = --output-format && test "$5" = json || exit 2
    shift 5
    : >options
    while test "$1" != --; do printf '%s\n' "$1" >>options; shift; test "$#" -ge 2 || exit 2; done
    test "$#" = 2 || exit 2
    printf %s "$2" >prompt
    case "$mode" in
        large) dd if=/dev/zero bs=65536 count=65; exit;;
        badjson) echo NOT-JSON; exit;;
        fail) error=true;;
        *) error=false;;
    esac
    printf '{"type":"result","subtype":"success","is_error":%s,"session_id":"%s"}\n' "$error" "$reply_id"
else
    test "$#" = 1 && test "$1" = app-server
    expect() { IFS= read -r msg; case "$msg" in *\"method\":\""$1"\"*) ;; *) exit 3;; esac; }
    approve() { echo '{"id":42,"method":"item/commandExecution/requestApproval"}'; if IFS= read -r msg; then echo yes >approved; fi; exit; }
    expect initialize
    echo '{"id":1,"result":{}}'
    expect initialized
    expect thread/resume
    case "$msg" in *\"threadId\":\""$sid"\"*) ;; *) exit 4;; esac
    if test "$mode" = approval-before; then approve; fi
    printf '{"id":2,"result":{"thread":{"id":"%s"}}}\n' "$reply_id"
    expect turn/start
    printf %s "$msg" >prompt
    echo '{"id":3,"result":{"turn":{"id":"turn1"}}}'
    if test "$mode" = approval-after; then approve; fi
    turn=turn1; status=completed
    if test "$mode" = bad-turn; then turn=other; fi
    if test "$mode" = bad-thread; then reply_id=other; fi
    if test "$mode" = fail; then status=failed; fi
    printf '{"method":"turn/completed","params":{"threadId":"%s","turn":{"id":"%s","status":"%s"}}}\n' "$reply_id" "$turn" "$status"
fi
test "$mode" != exit