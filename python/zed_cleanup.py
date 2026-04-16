#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Утилита для очистки Zed AI треда от мусора с сохранением важных сообщений.

Что удаляется / сжимается (только в старых диалогах):
  - Thinking блоки (внутренние размышления агента) — не нужны в контексте
  - tool_results от terminal/ssh_run/ssh_connect — обрезаются до MAX_TERMINAL_BYTES
  - tool_results от read_file — обрезаются до MAX_FILE_BYTES
  - tool_results от grep — обрезаются до MAX_GREP_BYTES
  - Дублирующиеся tool_results от read_file одного и того же файла — удаляются (оставляется последний)
  - reasoning_details — удаляются полностью
  - initial_project_snapshot — удаляется

Что остаётся нетронутым:
  - Последние KEEP_LAST_DIALOGS диалогов (пар User/Agent) — не трогаются вообще
  - Все User сообщения (текст пользователя)
  - Text блоки агента (ответы агента)
  - ToolUse блоки (вызовы инструментов — чтобы было понятно что делалось)
  - edit_file, spawn_agent, copy_path, move_path, create_directory tool_results — полностью

Использование:
  python zed_cleanup.py <input.json> <output.json> [--dry-run] [--keep=N]
"""

import json
import sys
from pathlib import Path

# Настройки — сколько байт оставлять от результатов инструментов
MAX_TERMINAL_BYTES = 2_000  # вывод терминала / ssh
MAX_FILE_BYTES = 3_000  # содержимое файла
MAX_GREP_BYTES = 2_000  # результаты grep
MAX_OTHER_BYTES = 1_000  # всё остальное

# Сколько последних диалогов (пар User → Agent) оставлять нетронутыми
KEEP_LAST_DIALOGS = 10

# Инструменты результаты которых сохраняются ПОЛНОСТЬЮ (они важны для контекста)
KEEP_FULL_TOOLS = {
    "edit_file",
    "create_directory",
    "copy_path",
    "move_path",
    "delete_path",
    "spawn_agent",
    "save_file",
    "restore_file_from_disk",
}

# Инструменты которые генерируют много мусорного вывода
TERMINAL_TOOLS = {"terminal", "ssh_run", "ssh_connect", "ssh_disconnect", "ssh_status"}
FILE_TOOLS = {"read_file"}
GREP_TOOLS = {"grep", "find_path"}


def truncate_content(content, max_bytes: int, label: str) -> str:
    """Обрезает строку до max_bytes с пометкой."""
    if isinstance(content, list):
        # content может быть списком блоков
        text = json.dumps(content, ensure_ascii=False)
    else:
        text = str(content) if content is not None else ""

    encoded = text.encode("utf-8")
    if len(encoded) <= max_bytes:
        return content  # не трогаем

    truncated = encoded[:max_bytes].decode("utf-8", errors="ignore")
    return (
        truncated
        + f"\n... [TRUNCATED by zed_cleanup: original {len(encoded):,} bytes, kept {max_bytes:,}] ..."
    )


def get_max_bytes(tool_name: str) -> int:
    if tool_name in TERMINAL_TOOLS:
        return MAX_TERMINAL_BYTES
    if tool_name in FILE_TOOLS:
        return MAX_FILE_BYTES
    if tool_name in GREP_TOOLS:
        return MAX_GREP_BYTES
    return MAX_OTHER_BYTES


def clean_tool_results(tool_results: dict, stats: dict) -> dict:
    """Очищает tool_results в одном Agent сообщении."""
    if not tool_results:
        return tool_results

    cleaned = {}
    for tid, tr in tool_results.items():
        tool_name = tr.get("tool_name", "unknown")

        if tool_name in KEEP_FULL_TOOLS:
            cleaned[tid] = tr
            continue

        max_bytes = get_max_bytes(tool_name)
        original_size = len(json.dumps(tr, ensure_ascii=False))

        new_tr = dict(tr)

        # Обрезаем поле content
        if "content" in new_tr and new_tr["content"] is not None:
            new_tr["content"] = truncate_content(
                new_tr["content"], max_bytes, tool_name
            )

        # Обрезаем поле output если есть
        if "output" in new_tr and new_tr["output"] is not None:
            new_tr["output"] = truncate_content(
                new_tr["output"], max_bytes // 2, tool_name
            )

        new_size = len(json.dumps(new_tr, ensure_ascii=False))
        saved = original_size - new_size
        if saved > 0:
            stats["tool_results_saved"] += saved
            stats["tool_results_truncated"] += 1

        cleaned[tid] = new_tr

    return cleaned


def find_last_read_file_results(messages: list) -> set:
    """
    Для каждого уникального файла (по аргументу path в ToolUse read_file)
    находим id последнего вызова. Все предыдущие дубли можно удалить.
    Возвращает set tool_use_id которые надо УДАЛИТЬ (все кроме последнего).
    """
    # Сначала строим карту: path -> [tool_use_id, ...]
    file_calls = {}  # path -> list of tool_use_id in order

    for m in messages:
        if not isinstance(m, dict):
            continue
        role = list(m.keys())[0]
        if role != "Agent":
            continue
        data = m[role]
        for block in data.get("content", []):
            if not isinstance(block, dict):
                continue
            if "ToolUse" not in block:
                continue
            tu = block["ToolUse"]
            if tu.get("name") != "read_file":
                continue
            # Парсим путь из raw_input
            try:
                inp = json.loads(tu.get("raw_input", "{}"))
                path = inp.get("path", "")
            except Exception:
                path = tu.get("raw_input", "")[:100]

            if path:
                file_calls.setdefault(path, []).append(tu["id"])

    # Все id кроме последнего — дубли
    to_remove = set()
    for path, ids in file_calls.items():
        if len(ids) > 1:
            to_remove.update(ids[:-1])  # удаляем все кроме последнего

    return to_remove


def find_protected_indices(messages: list, keep_last_dialogs: int) -> set:
    """
    Возвращает set индексов сообщений которые трогать нельзя —
    это последние keep_last_dialogs диалогов (считаем по User сообщениям).
    """
    if keep_last_dialogs <= 0:
        return set()

    # Находим индексы всех User сообщений
    user_indices = []
    for i, m in enumerate(messages):
        if not isinstance(m, dict):
            continue
        if "User" in m:
            user_indices.append(i)

    # Берём последние N User сообщений
    cutoff_user_indices = set(user_indices[-keep_last_dialogs:])

    if not cutoff_user_indices:
        return set()

    # Минимальный индекс User из защищённых
    min_protected = min(cutoff_user_indices)

    # Защищаем все сообщения начиная с этого индекса
    return set(range(min_protected, len(messages)))


def clean_messages(
    messages: list, stats: dict, keep_last_dialogs: int = KEEP_LAST_DIALOGS
) -> list:
    """Основная функция очистки сообщений."""
    duplicate_read_ids = find_last_read_file_results(messages)
    stats["duplicate_read_files"] = len(duplicate_read_ids)

    protected = find_protected_indices(messages, keep_last_dialogs)
    stats["protected_messages"] = len(protected)

    cleaned_messages = []

    for idx, m in enumerate(messages):
        # Последние N диалогов — не трогаем вообще
        if idx in protected:
            cleaned_messages.append(m)
            continue

        m = m  # просто для читаемости
        if not isinstance(m, dict):
            cleaned_messages.append(m)
            continue

        role = list(m.keys())[0]
        data = m[role]
        new_data = dict(data)

        # --- Удаляем reasoning_details ---
        if "reasoning_details" in new_data:
            size = len(json.dumps(new_data["reasoning_details"], ensure_ascii=False))
            stats["reasoning_details_saved"] += size
            del new_data["reasoning_details"]

        # --- Чистим content блоки ---
        new_content = []
        for block in new_data.get("content", []):
            if not isinstance(block, dict):
                new_content.append(block)
                continue

            block_type = list(block.keys())[0]

            # Удаляем Thinking блоки
            if block_type == "Thinking":
                size = len(json.dumps(block, ensure_ascii=False))
                stats["thinking_saved"] += size
                stats["thinking_removed"] += 1
                continue  # пропускаем

            # Удаляем RedactedThinking
            if block_type == "RedactedThinking":
                size = len(json.dumps(block, ensure_ascii=False))
                stats["thinking_saved"] += size
                continue

            # ToolUse для дублирующихся read_file — помечаем но оставляем
            # (удалять ToolUse нельзя — сломается структура, просто чистим result)
            new_content.append(block)

        new_data["content"] = new_content

        # --- Чистим tool_results ---
        if "tool_results" in new_data and new_data["tool_results"]:
            tr = new_data["tool_results"]

            # Удаляем результаты дублирующихся read_file
            filtered_tr = {}
            for tid, result in tr.items():
                if tid in duplicate_read_ids:
                    size = len(json.dumps(result, ensure_ascii=False))
                    stats["duplicate_saved"] += size
                    # Заменяем на заглушку вместо полного удаления
                    filtered_tr[tid] = {
                        "tool_use_id": result.get("tool_use_id", tid),
                        "tool_name": result.get("tool_name", "read_file"),
                        "is_error": False,
                        "content": "[REMOVED by zed_cleanup: duplicate read_file, kept only last occurrence]",
                        "output": None,
                    }
                else:
                    filtered_tr[tid] = result

            new_data["tool_results"] = clean_tool_results(filtered_tr, stats)

        cleaned_messages.append({role: new_data})

    return cleaned_messages


def cleanup(
    input_file: str,
    output_file: str,
    dry_run: bool = False,
    keep_last_dialogs: int = KEEP_LAST_DIALOGS,
):
    print(f"Читаем: {input_file}")
    d = json.load(open(input_file, encoding="utf-8"))

    original_size = len(json.dumps(d, ensure_ascii=False).encode("utf-8"))

    stats = {
        "thinking_saved": 0,
        "thinking_removed": 0,
        "reasoning_details_saved": 0,
        "tool_results_saved": 0,
        "tool_results_truncated": 0,
        "duplicate_saved": 0,
        "duplicate_read_files": 0,
    }

    print(f"Оставляем нетронутыми последние {keep_last_dialogs} диалогов")

    # Чистим сообщения
    d["messages"] = clean_messages(d["messages"], stats, keep_last_dialogs)

    # Пересчитываем request_token_usage — оставляем только те записи,
    # чьи id ещё присутствуют в сообщениях (остальные удалены очисткой)
    existing_user_ids = set()
    for m in d["messages"]:
        if isinstance(m, dict) and "User" in m:
            uid = m["User"].get("id")
            if uid:
                existing_user_ids.add(uid)

    old_rtu = d.get("request_token_usage", {})
    new_rtu = {k: v for k, v in old_rtu.items() if k in existing_user_ids}
    removed_rtu = len(old_rtu) - len(new_rtu)
    stats["token_usage_removed"] = removed_rtu

    # Пересчитываем cumulative из оставшихся записей
    cumulative = {
        "input_tokens": 0,
        "output_tokens": 0,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
    }
    for v in new_rtu.values():
        for key in cumulative:
            cumulative[key] += v.get(key, 0)

    d["request_token_usage"] = new_rtu
    d["cumulative_token_usage"] = cumulative

    stats["new_cumulative"] = cumulative

    # Удаляем initial_project_snapshot (большой снапшот проекта)
    if "initial_project_snapshot" in d and d["initial_project_snapshot"]:
        size = len(json.dumps(d["initial_project_snapshot"], ensure_ascii=False))
        stats["snapshot_saved"] = size
        d["initial_project_snapshot"] = None
    else:
        stats["snapshot_saved"] = 0

    new_size = len(json.dumps(d, ensure_ascii=False).encode("utf-8"))
    total_saved = original_size - new_size

    print()
    print("=== Статистика очистки ===")
    print(
        f"  Исходный размер       : {original_size:>12,} байт  ({original_size / 1024 / 1024:.2f} MB)"
    )
    print(
        f"  Итоговый размер       : {new_size:>12,} байт  ({new_size / 1024 / 1024:.2f} MB)"
    )
    print(
        f"  Сэкономлено           : {total_saved:>12,} байт  ({total_saved / 1024 / 1024:.2f} MB)  [{total_saved / original_size * 100:.1f}%]"
    )
    print()
    print(
        f"  Защищено сообщений (последние {keep_last_dialogs} диалогов): {stats['protected_messages']}"
    )
    print(f"  Thinking блоков удалено       : {stats['thinking_removed']}")
    print(f"  Thinking байт сэкономлено     : {stats['thinking_saved']:,}")
    print(f"  reasoning_details сэкономлено : {stats['reasoning_details_saved']:,}")
    print(f"  tool_results обрезано         : {stats['tool_results_truncated']}")
    print(f"  tool_results байт сэкономлено : {stats['tool_results_saved']:,}")
    print(f"  дублей read_file удалено      : {stats['duplicate_read_files']}")
    print(f"  дублей read_file байт сэконом.: {stats['duplicate_saved']:,}")
    print(f"  project_snapshot байт сэконом.: {stats['snapshot_saved']:,}")
    print(f"  token_usage записей удалено   : {stats['token_usage_removed']}")
    c = stats["new_cumulative"]
    print(
        f"  новый cumulative_token_usage  : input={c['input_tokens']:,}  output={c['output_tokens']:,}  cache_read={c['cache_read_input_tokens']:,}"
    )

    if dry_run:
        print()
        print("[dry-run] Файл не сохранён.")
        return

    pretty = json.dumps(d, ensure_ascii=False, indent=2)
    Path(output_file).write_text(pretty, encoding="utf-8")
    print()
    print(f"Сохранено в: {output_file}")


def main():
    args = sys.argv[1:]
    dry_run = "--dry-run" in args
    args = [a for a in args if a != "--dry-run"]

    keep_last_dialogs = KEEP_LAST_DIALOGS
    keep_args = [a for a in args if a.startswith("--keep=")]
    if keep_args:
        try:
            keep_last_dialogs = int(keep_args[0].split("=")[1])
        except ValueError:
            print(f"[!] Неверный формат --keep, используем {KEEP_LAST_DIALOGS}")
    args = [a for a in args if not a.startswith("--keep=")]

    if len(args) != 2:
        print(__doc__)
        sys.exit(0)

    input_file, output_file = args
    cleanup(input_file, output_file, dry_run, keep_last_dialogs)


if __name__ == "__main__":
    main()
