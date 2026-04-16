import json
from collections import Counter, defaultdict

d = json.load(open("1.json", encoding="utf-8"))
msgs = d["messages"]

tool_names = Counter()
size_by_type = defaultdict(int)
tool_result_total = 0

for m in msgs:
    if not isinstance(m, dict):
        continue
    role = list(m.keys())[0]
    data = m[role]

    for block in data.get("content", []):
        if not isinstance(block, dict):
            continue
        t = list(block.keys())[0]
        s = len(json.dumps(block, ensure_ascii=False))
        size_by_type[t] += s
        if t == "ToolUse":
            tool_names[block["ToolUse"].get("name", "?")] += 1

    for tid, tr in data.get("tool_results", {}).items():
        s = len(json.dumps(tr, ensure_ascii=False))
        tool_result_total += s
        tool_names[tr.get("tool_name", "?")] += 1

total = sum(size_by_type.values()) + tool_result_total
print(f"Всего сообщений: {len(msgs)}")
print()
print("Размеры по типам (в content):")
for k, v in sorted(size_by_type.items(), key=lambda x: -x[1]):
    print(f"  {k:25s}: {v:>10,} байт  ({v / total * 100:.1f}%)")
print(
    f"  {'tool_results':25s}: {tool_result_total:>10,} байт  ({tool_result_total / total * 100:.1f}%)"
)
print(f"  {'ИТОГО':25s}: {total:>10,} байт")
print()
print("Инструменты (вызовы + результаты):")
for k, v in tool_names.most_common():
    print(f"  {k}: {v}")
