#!/usr/bin/env bash
# §20.24: измерения производительности berimor — PSS (фактическая
# доля RAM по /proc/*/smaps_rollup) и time-to-first-render через pty
# (TUI живёт только в псевдотерминале — без него chat честно уходит в
# REPL и завершается по EOF, что и показал первый прогон скрипта).
# Использование: scripts/bench.sh [путь-к-бинарнику]
set -u

BIN="${1:-$HOME/.local/bin/berimor}"
RUNS=5

if [ ! -x "$BIN" ]; then
    echo "бинарник не найден: $BIN" >&2
    exit 1
fi

echo "== беримор: $( "$BIN" --version ) =="
echo "бинарник: $BIN ($(stat -c%s "$BIN" | numfmt --to=iec))"
echo "машина: $(uname -sr), $(nproc) потоков CPU, $(free -m | awk '/^Mem:/ {print $2}') МБ RAM"
echo

# --- PSS интерактивной TUI-сессии (pty через script, замер дочернего) --
echo "== PSS TUI-сессии (chat, pty, $RUNS прогонов) =="
for i in $(seq 1 "$RUNS"); do
    script -qec "stty rows 40 cols 120; exec $BIN" /dev/null >/dev/null 2>&1 &
    SCRIPT_PID=$!
    sleep 3
    # berimor — ребёнок script; ищем по имени и родителю
    CHILD=$(pgrep -P "$SCRIPT_PID" -f berimor | head -1)
    [ -z "$CHILD" ] && CHILD=$(pgrep -f "^$BIN" | head -1)
    if [ -n "$CHILD" ] && [ -r "/proc/$CHILD/smaps_rollup" ]; then
        PSS=$(awk '/^Pss:/ {print $2}' "/proc/$CHILD/smaps_rollup")
        RSS=$(awk '/^Rss:/ {print $2}' "/proc/$CHILD/smaps_rollup")
        echo "  прогон $i: PSS $((PSS / 1024)).$(( (PSS % 1024) * 10 / 1024 )) МБ (RSS $((RSS / 1024)) МБ)"
    else
        echo "  прогон $i: процесс не найден для замера"
    fi
    kill "$SCRIPT_PID" "$CHILD" 2>/dev/null
    wait "$SCRIPT_PID" 2>/dev/null
done
echo

# --- Холодный старт CLI-команд ------------------------------------------
echo "== холодный старт (мс, $RUNS прогонов) =="
for CMD in "--version" "--help"; do
    echo "  $BIN $CMD:"
    for i in $(seq 1 "$RUNS"); do
        START=$(date +%s%N)
        "$BIN" $CMD >/dev/null 2>&1
        END=$(date +%s%N)
        echo "    прогон $i: $(( (END - START) / 1000000 )) мс"
    done
done
echo

# --- Time-to-first-render: запуск TUI → первый байт в pty-логе ---------
echo "== time-to-first-render (pty, TUI, $RUNS прогонов) =="
for i in $(seq 1 "$RUNS"); do
    LOG=$(mktemp)
    START=$(date +%s%N)
    script -qec "stty rows 40 cols 120; exec $BIN" "$LOG" >/dev/null 2>&1 &
    SCRIPT_PID=$!
    ELAPSED=0
    while [ $ELAPSED -lt 10000 ]; do
        if [ -s "$LOG" ]; then
            END=$(date +%s%N)
            echo "  прогон $i: $(( (END - START) / 1000000 )) мс до первого байта рендера"
            break
        fi
        sleep 0.01
        ELAPSED=$((ELAPSED + 10))
    done
    [ $ELAPSED -ge 10000 ] && echo "  прогон $i: рендера нет за 10 с"
    kill "$SCRIPT_PID" 2>/dev/null
    pkill -P "$SCRIPT_PID" 2>/dev/null
    wait "$SCRIPT_PID" 2>/dev/null
    rm -f "$LOG"
done
