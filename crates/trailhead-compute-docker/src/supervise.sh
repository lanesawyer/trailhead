#!/bin/sh
# Supervises the programs `Compute::start` puts in a Docker sandbox, the way Sprites services are supervised.
# Each lives in $PROGRAMS/<name>: `run` is its command, `log` its output, `exits` one exit code per line.
#
#   supervise <name>   (re)starts one program and keeps restarting it when it exits
#   supervise          at container start: starts every program, then waits as the container's main process
set -u
PROGRAMS="$HOME/.trailhead/programs"

if [ $# -eq 0 ]; then
  mkdir -p "$PROGRAMS"
  # Pids from before a restart may now belong to other processes.
  rm -f "$PROGRAMS"/*/pid
  for dir in "$PROGRAMS"/*/; do
    [ -x "${dir}run" ] && setsid "$0" "$(basename "$dir")" &
  done
  while :; do sleep 3600; done
fi

dir="$PROGRAMS/$1"
if [ -f "$dir/pid" ]; then
  # Each supervisor leads its own process group, so this stops the program it was running too.
  kill -TERM -- "-$(cat "$dir/pid")" 2>/dev/null
fi
echo $$ > "$dir/pid"
: > "$dir/exits"
: > "$dir/log"
while :; do
  "$dir/run" >> "$dir/log" 2>&1
  echo "$?" >> "$dir/exits"
  sleep 1
done
