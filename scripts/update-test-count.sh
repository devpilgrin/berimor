#!/usr/bin/env bash
# Структурный фикс протухания бейджа тестов (2026-08-25, третий случай
# рассинхрона): единственное место, которое знает число, — прогон
# cargo test. Скрипт принимает число и синхронизирует бейдж и текст
# во всех README ×8. Использование:
#   scripts/update-test-count.sh           # сам гоняет тесты и считает
#   scripts/update-test-count.sh 1002      # число дано извне (CI)
set -eu

COUNT="${1:-}"
if [ -z "$COUNT" ]; then
    COUNT=$(
        "$HOME/.cargo/bin/cargo" test --workspace 2>&1 \
            | grep -oE '[0-9]+ passed' | grep -oE '^[0-9]+' \
            | awk '{s+=$1} END {print s}'
    )
fi
[ -n "$COUNT" ] || { echo "не удалось получить счётчик" >&2; exit 1; }

CHANGED=0
for f in README.md README.en.md README.de.md README.fr.md README.es.md README.zh-CN.md README.ja.md README.ko.md; do
    [ -f "$f" ] || continue
    # Бейдж shields: tests-NNN%20green
    sed -i -E "s|badge/tests-[0-9]+%20green-brightgreen|badge/tests-${COUNT}%20green-brightgreen|g" "$f"
    # Текстовое число в разделе дисциплины (7 локальных форм)
    sed -i -E "s|[0-9]{3,4}( теста| tests| Tests| テスト| 个测试|개 테스트)|${COUNT}\1|g" "$f"
    if ! git diff --quiet -- "$f" 2>/dev/null; then
        CHANGED=1
    fi
done
echo "счётчик тестов: $COUNT; README обновлены: $([ $CHANGED -eq 1 ] && echo да || echo нет)"
