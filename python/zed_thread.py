#!/usr/bin/env python3
# -*- coding: utf-8 -*-
"""
Утилита для чтения/записи тредов из Zed IDE SQLite базы данных.

Использование:
  python zed_thread.py read  "<thread_id>" "<output_file>"
  python zed_thread.py write "<thread_id>" "<input_file>"
"""

import io
import json
import sqlite3
import sys
from pathlib import Path

# Принудительно UTF-8 для stdout/stderr (особенно при редиректе в файл на Windows)
sys.stdout = io.TextIOWrapper(sys.stdout.buffer, encoding="utf-8", errors="replace")
sys.stderr = io.TextIOWrapper(sys.stderr.buffer, encoding="utf-8", errors="replace")

import platform

import zstandard as zstd


def _default_db_path() -> str:
    """Auto-detect threads.db path based on OS."""
    system = platform.system()
    if system == "Windows":
        base = os.environ.get("LOCALAPPDATA", "")
        return os.path.join(base, "Zed", "threads", "threads.db")
    elif system == "Darwin":
        home = os.path.expanduser("~")
        return os.path.join(
            home, "Library", "Application Support", "Zed", "threads", "threads.db"
        )
    else:  # Linux
        base = os.environ.get("XDG_DATA_HOME", os.path.expanduser("~/.local/share"))
        return os.path.join(base, "zed", "threads", "threads.db")


import os

DB_PATH = os.environ.get("ZED_THREADS_DB", _default_db_path())


def read_thread(thread_id: str, output_file: str):
    conn = sqlite3.connect(DB_PATH)
    try:
        cur = conn.cursor()
        cur.execute(
            "SELECT data_type, data FROM threads WHERE id = ? LIMIT 1", (thread_id,)
        )
        row = cur.fetchone()
        if row is None:
            print(f"[!] Тред с id '{thread_id}' не найден.")
            sys.exit(1)

        data_type, data = row

        if data_type == "zstd":
            dctx = zstd.ZstdDecompressor()
            raw = dctx.decompress(data, max_output_size=100 * 1024 * 1024)
        elif data_type == "json":
            raw = data if isinstance(data, bytes) else data.encode("utf-8")
        else:
            print(f"[!] Неизвестный тип данных: {data_type}")
            sys.exit(1)

        # Красиво форматируем JSON
        parsed = json.loads(raw)
        pretty = json.dumps(parsed, ensure_ascii=False, indent=2)

        Path(output_file).write_text(pretty, encoding="utf-8")
        print(f"[+] Тред '{thread_id}' сохранён в '{output_file}'")
        print(f"    Тип данных: {data_type}, размер: {len(raw)} байт")

    finally:
        conn.close()


def write_thread(thread_id: str, input_file: str):
    content = Path(input_file).read_text(encoding="utf-8")

    # Проверяем что JSON валидный
    try:
        json.loads(content)
    except json.JSONDecodeError as e:
        print(f"[!] Файл содержит невалидный JSON: {e}")
        sys.exit(1)

    cctx = zstd.ZstdCompressor(level=3)
    compressed = cctx.compress(content.encode("utf-8"))

    conn = sqlite3.connect(DB_PATH)
    try:
        cur = conn.cursor()
        cur.execute("SELECT id FROM threads WHERE id = ? LIMIT 1", (thread_id,))
        row = cur.fetchone()
        if row is None:
            print(f"[!] Тред с id '{thread_id}' не найден в базе. Запись отменена.")
            sys.exit(1)

        cur.execute(
            "UPDATE threads SET data = ?, data_type = ? WHERE id = ?",
            (compressed, "zstd", thread_id),
        )
        conn.commit()
        print(f"[+] Тред '{thread_id}' обновлён из файла '{input_file}'")
        print(f"    Размер сжатых данных: {len(compressed)} байт")

    finally:
        conn.close()


def list_threads():
    conn = sqlite3.connect(DB_PATH)
    try:
        cur = conn.cursor()
        cur.execute(
            "SELECT id, summary, updated_at FROM threads ORDER BY updated_at DESC"
        )
        rows = cur.fetchall()
        if not rows:
            print("База данных пуста.")
            return
        print(f"{'ID':<40} {'updated_at':<25} {'Заголовок'}")
        print("-" * 100)
        for tid, summary, updated_at in rows:
            print(f"{tid:<40} {updated_at:<25} {summary}")
    finally:
        conn.close()


def main():
    if len(sys.argv) < 2:
        print(__doc__)
        print("Доп. команда:")
        print("  python zed_thread.py list   — показать все треды")
        sys.exit(0)

    command = sys.argv[1].lower()

    if command == "list":
        list_threads()

    elif command == "read":
        if len(sys.argv) != 4:
            print(
                'Использование: python zed_thread.py read "<thread_id>" "<output_file>"'
            )
            sys.exit(1)
        read_thread(sys.argv[2], sys.argv[3])

    elif command == "write":
        if len(sys.argv) != 4:
            print(
                'Использование: python zed_thread.py write "<thread_id>" "<input_file>"'
            )
            sys.exit(1)
        write_thread(sys.argv[2], sys.argv[3])

    else:
        print(
            f"[!] Неизвестная команда: '{command}'. Используйте read, write или list."
        )
        sys.exit(1)


if __name__ == "__main__":
    main()
