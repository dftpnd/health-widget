#!/usr/bin/env python3
"""Потоковое распознавание речи для health-widget через faster-whisper.

Читает СЫРОЙ моно-PCM s16le 16000 Гц из stdin (его пишет Rust-канал после ресемплинга)
и печатает построчный JSON в stdout — только законченные фразы:

    {"final": "Запусти Kubernetes"}

Endpointing по энергии (пауза => конец фразы), инференс на GPU (CUDA), два рычага под
IT-термины: hotwords/initial_prompt при распознавании + словарь пост-коррекции.
Никакой сети в рантайме (модель кэшируется локально при установке).

Аргументы:
    argv[1] — имя или путь модели faster-whisper (напр. large-v3)
Окружение:
    WHISPER_DEVICE (cuda), WHISPER_COMPUTE (float16)
Файлы баз (рядом со скриптом, опциональны):
    it_hotwords.txt, it_corrections.tsv
"""
import os
import re
import sys
import json
import time
import threading
from typing import Iterable


def parse_hotwords(lines: Iterable[str], limit: int = 80) -> str:
    terms = []
    for line in lines:
        s = line.strip()
        if not s or s.startswith("#"):
            continue
        terms.append(s)
        if len(terms) >= limit:
            break
    return ", ".join(terms)


def parse_corrections(lines: Iterable[str]):
    pairs = []
    for line in lines:
        s = line.rstrip("\n")
        if not s or s.startswith("#") or "\t" not in s:
            continue
        wrong, right = s.split("\t", 1)
        wrong, right = wrong.strip(), right.strip()
        if not wrong or not right:
            continue
        rx = re.compile(r"(?<!\w)" + re.escape(wrong) + r"(?!\w)", re.IGNORECASE)
        pairs.append((rx, right))
    return pairs


def apply_corrections(text: str, pairs) -> str:
    for rx, right in pairs:
        text = rx.sub(right, text)
    return text


def load_hotwords(path: str) -> str:
    try:
        with open(path, encoding="utf-8") as f:
            return parse_hotwords(f)
    except OSError:
        return ""


def load_corrections(path: str):
    try:
        with open(path, encoding="utf-8") as f:
            return parse_corrections(f)
    except OSError:
        return []
