#!/usr/bin/env python3
"""
Claude Sessions Dashboard — a local, private web dashboard for your Claude Code sessions.
# gotta catch 'em all

What it does
------------
Serves a small web app that shows your live Claude Code sessions (from
`claude agents --json`) enriched with data parsed from the JSONL transcript files
under ~/.claude/projects, plus Cursor agent transcripts under ~/.cursor/projects,
and a gamified "season" panel (XP / level / streak /
achievements / 14-day activity calendar) computed from the last 30 days of activity
across ALL sessions.

Endpoints
---------
  GET /               -> serves index.html (re-read from disk each request)
  GET /api/sessions   -> the JSON contract consumed by the frontend
  anything else       -> 404

Privacy stance
--------------
This is a LOCAL tool. It binds to 127.0.0.1 ONLY (never 0.0.0.0), and rejects any
request whose Host header is not localhost/127.0.0.1 with a 403. Your transcripts and
session activity never leave your machine. No auth is added beyond loopback binding
because the data is only exposed to processes on this host.

Usage
-----
  python3 dashboard.py [--port 8765] [--no-open]
"""

import argparse
import glob
import base64
import hashlib
import json
import math
import os
import re
import secrets
import shlex
import signal
import sqlite3
import subprocess
import sys
import tempfile
import threading
import time
import webbrowser
from datetime import datetime, timezone, timedelta, date
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import arena
import music
import worksignals

# HQ 2.5 local proxy extension modules (ext_<feature>.py: GET/POST maps and an
# optional start(ctx)). Each import is guarded so one broken module never takes
# the HQ down.
EXT = []
for _ext_name in ("ext_skills", "ext_loot", "ext_prestige", "ext_maps", "ext_cups", "ext_boss"):
    try:
        EXT.append(__import__(_ext_name))
    except Exception:
        pass

APP_VERSION = "2.5.0"   # Make + compete: editors, gallery, cups, boss, loot, skills

# --------------------------------------------------------------------------- #
# Paths / constants
# --------------------------------------------------------------------------- #

HERE = os.path.dirname(os.path.abspath(__file__))
INDEX_HTML = os.path.join(HERE, "index.html")
UI_DIR = os.path.join(HERE, "ui")
# The Claude HQ mark: one still frame of the "searching" thinking orb (the dotted
# globe the sidebar logo animates), rendered from the vendored thinking-orbs engine
# (RareFormLabs, MIT) and flattened to SVG dots on a dark tile.
ICON_SVG = ('<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 512 512"><rect width="512" height="512" rx="112" fill="#111217"/><circle cx="247.3" cy="255" r="2.9" fill="#616161" fill-opacity="0.45"/><circle cx="281.7" cy="255.9" r="2.9" fill="#626262" fill-opacity="0.45"/><circle cx="213.3" cy="257.6" r="2.9" fill="#636363" fill-opacity="0.45"/><circle cx="267" cy="299.5" r="2.9" fill="#646464" fill-opacity="0.45"/><circle cx="258.2" cy="210.6" r="2.9" fill="#646464" fill-opacity="0.45"/><circle cx="233.2" cy="300.2" r="2.9" fill="#646464" fill-opacity="0.45"/><circle cx="224.6" cy="211.9" r="2.9" fill="#656565" fill-opacity="0.45"/><circle cx="291.6" cy="212.3" r="2.9" fill="#656565" fill-opacity="0.45"/><circle cx="314.7" cy="260.2" r="2.9" fill="#666666" fill-opacity="0.45"/><circle cx="300" cy="302.8" r="2.9" fill="#676767" fill-opacity="0.45"/><circle cx="201.2" cy="304.9" r="2.9" fill="#696969" fill-opacity="0.45"/><circle cx="181.8" cy="263.6" r="2.9" fill="#696969" fill-opacity="0.45"/><circle cx="192.6" cy="216.3" r="2.9" fill="#696969" fill-opacity="0.45"/><circle cx="323.4" cy="217" r="2.9" fill="#6a6a6a" fill-opacity="0.45"/><circle cx="245.7" cy="340.4" r="2.9" fill="#6c6c6c" fill-opacity="0.45"/><circle cx="258.2" cy="169.9" r="2.9" fill="#6c6c6c" fill-opacity="0.45"/><circle cx="278.8" cy="341.5" r="2.9" fill="#6d6d6d" fill-opacity="0.45"/><circle cx="344.3" cy="267.7" r="2.9" fill="#6d6d6d" fill-opacity="0.45"/><circle cx="224.6" cy="171.3" r="2.9" fill="#6d6d6d" fill-opacity="0.45"/><circle cx="329.3" cy="309.9" r="2.9" fill="#6e6e6e" fill-opacity="0.45"/><circle cx="291.6" cy="171.7" r="2.9" fill="#6e6e6e" fill-opacity="0.45"/><circle cx="163.4" cy="223.4" r="2.9" fill="#707070" fill-opacity="0.45"/><circle cx="214.1" cy="344.8" r="2.9" fill="#707070" fill-opacity="0.45"/><circle cx="173.7" cy="313.2" r="2.9" fill="#717171" fill-opacity="0.45"/><circle cx="352" cy="224.5" r="2.9" fill="#717171" fill-opacity="0.45"/><circle cx="192.6" cy="175.6" r="2.9" fill="#717171" fill-opacity="0.45"/><circle cx="154.7" cy="272.6" r="2.9" fill="#727272" fill-opacity="0.45"/><circle cx="323.4" cy="176.4" r="2.9" fill="#727272" fill-opacity="0.45"/><circle cx="308.5" cy="347.8" r="2.9" fill="#737373" fill-opacity="0.45"/><circle cx="368.7" cy="278" r="2.9" fill="#777777" fill-opacity="0.45"/><circle cx="352.8" cy="320.3" r="2.9" fill="#777777" fill-opacity="0.45"/><circle cx="163.4" cy="182.8" r="2.9" fill="#787878" fill-opacity="0.45"/><circle cx="250.6" cy="374.4" r="2.9" fill="#787878" fill-opacity="0.45"/><circle cx="189" cy="353.9" r="2.9" fill="#797979" fill-opacity="0.45"/><circle cx="138.7" cy="233" r="3" fill="#797979" fill-opacity="0.45"/><circle cx="247.3" cy="136.4" r="3" fill="#797979" fill-opacity="0.45"/><circle cx="352" cy="183.9" r="3" fill="#797979" fill-opacity="0.45"/><circle cx="281.7" cy="137.3" r="3" fill="#7a7a7a" fill-opacity="0.45"/><circle cx="376.2" cy="234.5" r="3" fill="#7a7a7a" fill-opacity="0.45"/><circle cx="152.8" cy="324.5" r="3" fill="#7b7b7b" fill-opacity="0.45"/><circle cx="213.3" cy="139" r="3.1" fill="#7c7c7c" fill-opacity="0.45"/><circle cx="283.3" cy="378.2" r="3.1" fill="#7c7c7c" fill-opacity="0.45"/><circle cx="133.4" cy="284.1" r="3.1" fill="#7c7c7c" fill-opacity="0.45"/><circle cx="330.1" cy="358.4" r="3.1" fill="#7d7d7d" fill-opacity="0.45"/><circle cx="314.7" cy="141.6" r="3.2" fill="#7e7e7e" fill-opacity="0.45"/><circle cx="221.1" cy="381.4" r="3.2" fill="#7f7f7f" fill-opacity="0.45"/><circle cx="138.7" cy="192.4" r="3.3" fill="#818181" fill-opacity="0.45"/><circle cx="181.8" cy="145" r="3.3" fill="#818181" fill-opacity="0.45"/><circle cx="386.6" cy="290.5" r="3.3" fill="#828282" fill-opacity="0.45"/><circle cx="376.2" cy="193.8" r="3.3" fill="#838383" fill-opacity="0.45"/><circle cx="368.3" cy="332.9" r="3.4" fill="#838383" fill-opacity="0.45"/><circle cx="119.4" cy="244.7" r="3.4" fill="#848484" fill-opacity="0.45"/><circle cx="174" cy="366.5" r="3.4" fill="#848484" fill-opacity="0.45"/><circle cx="344.3" cy="149.1" r="3.5" fill="#858585" fill-opacity="0.45"/><circle cx="394.7" cy="246.3" r="3.5" fill="#858585" fill-opacity="0.45"/><circle cx="140.3" cy="337.8" r="3.6" fill="#888888" fill-opacity="0.45"/><circle cx="300" cy="390.7" r="3.6" fill="#888888" fill-opacity="0.45"/><circle cx="256" cy="398.7" r="3.6" fill="#898989" fill-opacity="0.45"/><circle cx="119.2" cy="297.3" r="3.6" fill="#898989" fill-opacity="0.45"/><circle cx="340.5" cy="371.7" r="3.6" fill="#898989" fill-opacity="0.45"/><circle cx="154.7" cy="154" r="3.6" fill="#8a8a8a" fill-opacity="0.45"/><circle cx="267" cy="112.5" r="3.6" fill="#8a8a8a" fill-opacity="0.45"/><circle cx="233.2" cy="113.3" r="3.7" fill="#8a8a8a" fill-opacity="0.45"/><circle cx="212" cy="395.2" r="3.7" fill="#8c8c8c" fill-opacity="0.45"/><circle cx="119.4" cy="204.1" r="3.7" fill="#8c8c8c" fill-opacity="0.45"/><circle cx="300" cy="115.9" r="3.8" fill="#8d8d8d" fill-opacity="0.45"/><circle cx="394.7" cy="205.7" r="3.8" fill="#8e8e8e" fill-opacity="0.45"/><circle cx="368.7" cy="159.4" r="3.8" fill="#8f8f8f" fill-opacity="0.45"/><circle cx="201.2" cy="118" r="3.8" fill="#8f8f8f" fill-opacity="0.45"/><circle cx="396.9" cy="304.4" r="3.9" fill="#8f8f8f" fill-opacity="0.45"/><circle cx="106.4" cy="257.8" r="3.9" fill="#909090" fill-opacity="0.45"/><circle cx="374.8" cy="347" r="3.9" fill="#909090" fill-opacity="0.45"/><circle cx="171.5" cy="380.4" r="4" fill="#919191" fill-opacity="0.45"/><circle cx="406.7" cy="259.6" r="4" fill="#929292" fill-opacity="0.45"/><circle cx="329.3" cy="123" r="4" fill="#949494" fill-opacity="0.45"/><circle cx="133.4" cy="165.5" r="4.1" fill="#949494" fill-opacity="0.45"/><circle cx="290.9" cy="404.5" r="4.1" fill="#959595" fill-opacity="0.45"/><circle cx="137.2" cy="352" r="4.1" fill="#959595" fill-opacity="0.45"/><circle cx="113" cy="311.7" r="4.1" fill="#969696" fill-opacity="0.45"/><circle cx="338" cy="385.7" r="4.2" fill="#969696" fill-opacity="0.45"/><circle cx="173.7" cy="126.3" r="4.2" fill="#979797" fill-opacity="0.45"/><circle cx="228.7" cy="407.7" r="4.2" fill="#989898" fill-opacity="0.45"/><circle cx="106.4" cy="217.2" r="4.2" fill="#999999" fill-opacity="0.45"/><circle cx="406.7" cy="219" r="4.3" fill="#9a9a9a" fill-opacity="0.45"/><circle cx="386.6" cy="171.9" r="4.3" fill="#9a9a9a" fill-opacity="0.45"/><circle cx="261.4" cy="411.5" r="4.4" fill="#9b9b9b" fill-opacity="0.45"/><circle cx="245.7" cy="100.3" r="4.4" fill="#9d9d9d" fill-opacity="0.45"/><circle cx="399" cy="318.9" r="4.4" fill="#9d9d9d" fill-opacity="0.45"/><circle cx="352.8" cy="133.3" r="4.4" fill="#9d9d9d" fill-opacity="0.45"/><circle cx="100.5" cy="271.8" r="4.4" fill="#9d9d9d" fill-opacity="0.45"/><circle cx="278.8" cy="101.3" r="4.5" fill="#9e9e9e" fill-opacity="0.45"/><circle cx="371.7" cy="361.2" r="4.5" fill="#9e9e9e" fill-opacity="0.45"/><circle cx="181.9" cy="393.7" r="4.5" fill="#9e9e9e" fill-opacity="0.45"/><circle cx="411.7" cy="273.7" r="4.5" fill="#9f9f9f" fill-opacity="0.45"/><circle cx="214.1" cy="104.6" r="4.6" fill="#a1a1a1" fill-opacity="0.45"/><circle cx="119.2" cy="178.8" r="4.6" fill="#a1a1a1" fill-opacity="0.45"/><circle cx="152.8" cy="137.5" r="4.6" fill="#a1a1a1" fill-opacity="0.45"/><circle cx="323" cy="398.2" r="4.6" fill="#a2a2a2" fill-opacity="0.45"/><circle cx="143.7" cy="366" r="4.6" fill="#a2a2a2" fill-opacity="0.45"/><circle cx="308.5" cy="107.6" r="4.7" fill="#a4a4a4" fill-opacity="0.45"/><circle cx="115.1" cy="326.2" r="4.7" fill="#a4a4a4" fill-opacity="0.45"/><circle cx="100.5" cy="231.2" r="4.8" fill="#a6a6a6" fill-opacity="0.45"/><circle cx="411.7" cy="233" r="4.8" fill="#a7a7a7" fill-opacity="0.45"/><circle cx="396.9" cy="185.8" r="4.9" fill="#a8a8a8" fill-opacity="0.45"/><circle cx="203.5" cy="404.4" r="4.9" fill="#a8a8a8" fill-opacity="0.45"/><circle cx="368.3" cy="146" r="4.9" fill="#a9a9a9" fill-opacity="0.45"/><circle cx="189" cy="113.8" r="4.9" fill="#a9a9a9" fill-opacity="0.45"/><circle cx="359.2" cy="374.5" r="5" fill="#aaaaaa" fill-opacity="0.45"/><circle cx="392.8" cy="333.2" r="5" fill="#ababab" fill-opacity="0.45"/><circle cx="297.9" cy="407.4" r="5" fill="#ababab" fill-opacity="0.45"/><circle cx="101.9" cy="286" r="5" fill="#ababab" fill-opacity="0.45"/><circle cx="409.4" cy="287.9" r="5.1" fill="#acacac" fill-opacity="0.45"/><circle cx="330.1" cy="118.3" r="5.1" fill="#aeaeae" fill-opacity="0.45"/><circle cx="140.3" cy="150.8" r="5.1" fill="#aeaeae" fill-opacity="0.45"/><circle cx="233.2" cy="410.7" r="5.4" fill="#aeaeae" fill-opacity="0.51"/><circle cx="159.2" cy="378.7" r="5.1" fill="#aeaeae" fill-opacity="0.45"/><circle cx="113" cy="193.1" r="5.1" fill="#aeaeae" fill-opacity="0.45"/><circle cx="266.3" cy="411.7" r="5.4" fill="#afafaf" fill-opacity="0.5"/><circle cx="250.6" cy="100.5" r="5.2" fill="#b0b0b0" fill-opacity="0.45"/><circle cx="125.4" cy="340.1" r="5.2" fill="#b1b1b1" fill-opacity="0.45"/><circle cx="101.9" cy="245.4" r="5.3" fill="#b3b3b3" fill-opacity="0.45"/><circle cx="283.3" cy="104.3" r="5.3" fill="#b4b4b4" fill-opacity="0.45"/><circle cx="409.4" cy="247.2" r="5.4" fill="#b5b5b5" fill-opacity="0.45"/><circle cx="338.3" cy="385.7" r="5.4" fill="#b5b5b5" fill-opacity="0.45"/><circle cx="174" cy="126.3" r="5.4" fill="#b5b5b5" fill-opacity="0.45"/><circle cx="399" cy="200.3" r="5.4" fill="#b5b5b5" fill-opacity="0.45"/><circle cx="374.8" cy="160" r="5.5" fill="#b6b6b6" fill-opacity="0.45"/><circle cx="221.1" cy="107.5" r="5.5" fill="#b7b7b7" fill-opacity="0.45"/><circle cx="378.6" cy="346.5" r="5.5" fill="#b7b7b7" fill-opacity="0.45"/><circle cx="110.4" cy="299.8" r="5.5" fill="#b8b8b8" fill-opacity="0.45"/><circle cx="182.7" cy="389" r="5.7" fill="#b8b8b8" fill-opacity="0.48"/><circle cx="399.9" cy="301.5" r="5.6" fill="#b9b9b9" fill-opacity="0.45"/><circle cx="340.5" cy="131.6" r="5.6" fill="#bababa" fill-opacity="0.45"/><circle cx="137.2" cy="165" r="5.6" fill="#bbbbbb" fill-opacity="0.45"/><circle cx="115.1" cy="207.6" r="5.7" fill="#bcbcbc" fill-opacity="0.45"/><circle cx="310.8" cy="394" r="5.8" fill="#bdbdbd" fill-opacity="0.48"/><circle cx="143.3" cy="352.6" r="5.7" fill="#bdbdbd" fill-opacity="0.46"/><circle cx="212" cy="396.1" r="6.5" fill="#bfbfbf" fill-opacity="0.59"/><circle cx="300" cy="116.8" r="5.8" fill="#bfbfbf" fill-opacity="0.45"/><circle cx="110.4" cy="259.2" r="5.8" fill="#c0c0c0" fill-opacity="0.45"/><circle cx="278.8" cy="398.7" r="6.6" fill="#c1c1c1" fill-opacity="0.58"/><circle cx="399.9" cy="260.9" r="5.9" fill="#c2c2c2" fill-opacity="0.45"/><circle cx="245" cy="399.5" r="7.1" fill="#c2c2c2" fill-opacity="0.67"/><circle cx="357.3" cy="358" r="5.9" fill="#c2c2c2" fill-opacity="0.45"/><circle cx="171.5" cy="140.3" r="5.9" fill="#c2c2c2" fill-opacity="0.45"/><circle cx="392.8" cy="214.7" r="6" fill="#c3c3c3" fill-opacity="0.45"/><circle cx="256" cy="113.3" r="6" fill="#c3c3c3" fill-opacity="0.45"/><circle cx="125.8" cy="312.4" r="6" fill="#c4c4c4" fill-opacity="0.45"/><circle cx="212" cy="121.3" r="6" fill="#c4c4c4" fill-opacity="0.45"/><circle cx="371.7" cy="174.2" r="6" fill="#c4c4c4" fill-opacity="0.45"/><circle cx="383.7" cy="314" r="6.1" fill="#c5c5c5" fill-opacity="0.45"/><circle cx="167.7" cy="362.9" r="6.4" fill="#c6c6c6" fill-opacity="0.5"/><circle cx="338" cy="145.5" r="6.1" fill="#c7c7c7" fill-opacity="0.45"/><circle cx="143.7" cy="179.1" r="6.2" fill="#c8c8c8" fill-opacity="0.45"/><circle cx="125.4" cy="221.5" r="6.2" fill="#c9c9c9" fill-opacity="0.45"/><circle cx="330.2" cy="367" r="6.4" fill="#cacaca" fill-opacity="0.48"/><circle cx="125.8" cy="271.8" r="6.4" fill="#cccccc" fill-opacity="0.45"/><circle cx="290.9" cy="130.6" r="6.4" fill="#cccccc" fill-opacity="0.45"/><circle cx="383.7" cy="273.3" r="6.4" fill="#cdcdcd" fill-opacity="0.45"/><circle cx="197.3" cy="370.4" r="7.4" fill="#cdcdcd" fill-opacity="0.64"/><circle cx="147.3" cy="323.4" r="6.6" fill="#cecece" fill-opacity="0.48"/><circle cx="181.9" cy="153.6" r="6.5" fill="#cfcfcf" fill-opacity="0.45"/><circle cx="361.6" cy="324.7" r="6.5" fill="#cfcfcf" fill-opacity="0.46"/><circle cx="378.6" cy="227.9" r="6.5" fill="#cfcfcf" fill-opacity="0.45"/><circle cx="228.7" cy="133.8" r="6.8" fill="#cfcfcf" fill-opacity="0.51"/><circle cx="298.7" cy="373" r="7.1" fill="#d0d0d0" fill-opacity="0.58"/><circle cx="359.2" cy="187.5" r="6.5" fill="#d0d0d0" fill-opacity="0.45"/><circle cx="230.3" cy="374.7" r="8.3" fill="#d2d2d2" fill-opacity="0.79"/><circle cx="264.7" cy="375.6" r="8.1" fill="#d2d2d2" fill-opacity="0.75"/><circle cx="323" cy="158.1" r="6.6" fill="#d3d3d3" fill-opacity="0.45"/><circle cx="261.4" cy="137.6" r="8" fill="#d3d3d3" fill-opacity="0.71"/><circle cx="159.2" cy="191.7" r="6.7" fill="#d4d4d4" fill-opacity="0.46"/><circle cx="143.3" cy="234" r="6.8" fill="#d5d5d5" fill-opacity="0.46"/><circle cx="173.8" cy="332.2" r="7.4" fill="#d6d6d6" fill-opacity="0.58"/><circle cx="147.3" cy="282.8" r="6.9" fill="#d6d6d6" fill-opacity="0.49"/><circle cx="334.5" cy="333.1" r="7" fill="#d7d7d7" fill-opacity="0.49"/><circle cx="361.6" cy="284.1" r="6.8" fill="#d7d7d7" fill-opacity="0.46"/><circle cx="203.5" cy="164.2" r="7.3" fill="#d9d9d9" fill-opacity="0.53"/><circle cx="357.3" cy="239.4" r="6.9" fill="#dadada" fill-opacity="0.45"/><circle cx="338.3" cy="198.8" r="7" fill="#dbdbdb" fill-opacity="0.46"/><circle cx="297.9" cy="167.2" r="7.2" fill="#dcdcdc" fill-opacity="0.5"/><circle cx="204.2" cy="338.3" r="8.7" fill="#dcdcdc" fill-opacity="0.78"/><circle cx="303.7" cy="338.9" r="7.8" fill="#dcdcdc" fill-opacity="0.61"/><circle cx="182.7" cy="202.1" r="7.5" fill="#dedede" fill-opacity="0.54"/><circle cx="173.8" cy="291.5" r="7.9" fill="#dedede" fill-opacity="0.61"/><circle cx="167.7" cy="244.3" r="7.5" fill="#dfdfdf" fill-opacity="0.54"/><circle cx="233.2" cy="170.5" r="9.2" fill="#dfdfdf" fill-opacity="0.85"/><circle cx="237" cy="341.6" r="9.5" fill="#dfdfdf" fill-opacity="0.91"/><circle cx="270.7" cy="341.8" r="9" fill="#dfdfdf" fill-opacity="0.81"/><circle cx="334.5" cy="292.5" r="7.4" fill="#dfdfdf" fill-opacity="0.5"/><circle cx="266.3" cy="171.6" r="8.9" fill="#e0e0e0" fill-opacity="0.79"/><circle cx="330.2" cy="248.4" r="7.5" fill="#e2e2e2" fill-opacity="0.49"/><circle cx="310.8" cy="207.1" r="7.6" fill="#e3e3e3" fill-opacity="0.52"/><circle cx="204.2" cy="297.7" r="9.3" fill="#e4e4e4" fill-opacity="0.83"/><circle cx="212" cy="209.2" r="9.1" fill="#e5e5e5" fill-opacity="0.79"/><circle cx="303.7" cy="298.3" r="8.3" fill="#e5e5e5" fill-opacity="0.63"/><circle cx="197.3" cy="251.8" r="9" fill="#e6e6e6" fill-opacity="0.76"/><circle cx="278.8" cy="211.8" r="9" fill="#e7e7e7" fill-opacity="0.76"/><circle cx="237" cy="300.9" r="10.2" fill="#e7e7e7" fill-opacity="0.97"/><circle cx="270.7" cy="301.1" r="9.6" fill="#e7e7e7" fill-opacity="0.86"/><circle cx="245" cy="212.5" r="10.2" fill="#e8e8e8" fill-opacity="0.98"/><circle cx="298.7" cy="254.4" r="8.5" fill="#e8e8e8" fill-opacity="0.65"/><circle cx="230.3" cy="256.1" r="10.3" fill="#eaeaea" fill-opacity="0.98"/><circle cx="264.7" cy="257" r="10" fill="#eaeaea" fill-opacity="0.91"/></svg>')
PROJECTS_DIR = os.path.expanduser("~/.claude/projects")
# Cursor agent chats. Parent sessions only:
#   ~/.cursor/projects/<project-slug>/agent-transcripts/<uuid>/<uuid>.jsonl
# Subagent transcripts live one directory deeper and are not separate sessions.
CURSOR_PROJECTS_DIR = os.path.expanduser("~/.cursor/projects")
# A Cursor transcript still being written is "working". One touched in the last
# 12 hours stays in Idle, so a chat you had earlier today sits next to Claude
# instead of inside the collapsed archive. Older files join that archive.
CURSOR_WORKING_SECS = 120
CURSOR_IDLE_SECS = 12 * 3600
# Chat headings ("Extend chat data support") live here, not in the jsonl.
# composerHeaders is a small table; the rest of this file is Cursor's own state
# and is never scanned. Read-only, and only on this machine.
CURSOR_STATE_DB = os.path.expanduser(
    "~/Library/Application Support/Cursor/User/globalStorage/state.vscdb")

# Soundboard: clips live on the Arena host, but a local ./sounds dir (if it has
# any audio files) wins -- that's how you audition clips before they're on the
# VM. Both the dir and the files are git-ignored, so nothing reaches the public
# repo. Adding a sound is just dropping a file in; no code change.
LOCAL_SOUNDS_DIR = os.path.join(HERE, "sounds")
SOUND_TYPES = {".ogg": "audio/ogg", ".mp3": "audio/mpeg", ".wav": "audio/wav",
               ".m4a": "audio/mp4", ".webm": "audio/webm"}
# Upload ceiling for POST /api/arena/sounds; matches the backend's default.
MAX_SOUND_UPLOAD = 5 * 1024 * 1024


def local_sounds():
    """[{name,file,size}] for audio files in ./sounds, or [] if the dir is
    absent/empty. `name` is the filename without extension (the label)."""
    out = []
    try:
        for fn in sorted(os.listdir(LOCAL_SOUNDS_DIR)):
            ext = os.path.splitext(fn)[1].lower()
            p = os.path.join(LOCAL_SOUNDS_DIR, fn)
            if ext in SOUND_TYPES and os.path.isfile(p):
                out.append({"name": os.path.splitext(fn)[0], "file": fn,
                            "size": os.path.getsize(p)})
    except FileNotFoundError:
        pass
    return out


def receive_sound_upload(body):
    """Accept a browser upload ({name, data:<base64>}) and store the clip. If the
    Arena host is paired the clip goes there (so everyone hears it); otherwise it
    lands in the local ./sounds dir. Returns (status, json)."""
    name = body.get("name")
    data_b64 = body.get("data")
    if not isinstance(name, str) or not name.strip():
        return 400, {"error": "name required"}
    if not isinstance(data_b64, str) or not data_b64:
        return 400, {"error": "file data required"}

    base = os.path.basename(name).strip()
    ext = os.path.splitext(base)[1].lower()
    if (not base or base.startswith(".") or "/" in base or "\\" in base
            or ".." in base or ext not in SOUND_TYPES):
        return 400, {"error": "must be an audio file (%s)"
                     % ", ".join(sorted(SOUND_TYPES))}

    try:
        raw = base64.b64decode(data_b64, validate=True)
    except Exception:
        return 400, {"error": "invalid file encoding"}
    if not raw:
        return 400, {"error": "empty file"}
    if len(raw) > MAX_SOUND_UPLOAD:
        return 413, {"error": "file too large (max %d MB)"
                     % (MAX_SOUND_UPLOAD // (1024 * 1024))}

    # Paired -> push to the Arena host, the shared machine everyone plays from.
    if arena.is_paired():
        try:
            code, resp = arena.upload_sound(base, raw, SOUND_TYPES[ext])
        except Exception as e:
            return 502, {"error": "arena upload failed: %s" % e}
        # arena returns 0 when the host is unreachable; never emit status 0.
        return (code or 502), resp

    # Not paired -> keep it on this machine (dev / single-user).
    try:
        os.makedirs(LOCAL_SOUNDS_DIR, exist_ok=True)
        dest = os.path.join(LOCAL_SOUNDS_DIR, base)
        if (os.path.realpath(os.path.dirname(dest))
                != os.path.realpath(LOCAL_SOUNDS_DIR)):
            return 400, {"error": "bad name"}
        if os.path.exists(dest):
            return 409, {"error": "a sound with that name already exists"}
        tmp = dest + ".part"
        with open(tmp, "wb") as f:
            f.write(raw)
        os.replace(tmp, dest)
    except Exception as e:
        return 500, {"error": "could not save: %s" % e}
    return 201, {"name": os.path.splitext(base)[0], "file": base,
                 "size": len(raw), "source": "local"}

# CSRF: a per-process token, injected into index.html (replacing the __HQ_CSRF__
# placeholder) so only a same-origin page can read it and echo it back on POSTs.
CSRF_TOKEN = secrets.token_hex(16)
CSRF_PLACEHOLDER = "__HQ_CSRF__"
SERVER_PORT = 8765  # set for real in main(); used for Origin validation

# launchd auto-start
LAUNCH_LABEL = "com.claudehq.dashboard"
LAUNCH_PLIST = os.path.expanduser(
    "~/Library/LaunchAgents/%s.plist" % LAUNCH_LABEL)

# Starter creatures — stable pick per sessionId via hash.
CREATURES = [
    ("⚡🐭", "Pikachu"),
    ("🔥🦎", "Charmander"),
    ("💧🐢", "Squirtle"),
    ("🌱🐸", "Bulbasaur"),
    ("💨🐉", "Dragonite"),
    ("💦🦆", "Psyduck"),
    ("🌙🦊", "Umbreon"),
    ("🪨🐛", "Geodude"),
]

# --------------------------------------------------------------------------- #
# ORIGINAL monster species (invented — NOT real Pokemon). Stable, ordered list
# of 48. A session maps to a species by hash(sessionId) % 48. The frontend draws
# a deterministic pixel sprite from seed + typeHue + stage + shiny.
# --------------------------------------------------------------------------- #
SPECIES_NAMES = [
    "Emberpup", "Aquafin", "Sprigling", "Voltkit", "Mystifox", "Boulderbug",
    "Frostnib", "Umbracat", "Pebblemol", "Cindermouse", "Tidewhorl", "Mossling",
    "Sparkfly", "Dreamowl", "Craghorn", "Glacimini", "Nocturnip", "Duskmoth",
    "Lumazee", "Brambeak", "Coralux", "Zaptadpole", "Gloamkit", "Terrapawn",
    "Flarelynx", "Marisprite", "Thornvale", "Ionbuzz", "Chronowisp", "Dracowyrm",
    "Rimepix", "Faewhisk",
    "Cindertail", "Brinewisp", "Fernbud", "Voltling", "Psybloom", "Stonecrag",
    "Frostmane", "Shadowpip", "Wispkit", "Drakelet", "Pixiewing", "Magmaturtle",
    "Rippletusk", "Vinecoil", "Sparkmoth", "Glimmerfawn",
]
# Fixed type per species (index-aligned with SPECIES_NAMES).
SPECIES_TYPES = [
    "fire", "water", "grass", "electric", "psychic", "rock",
    "ice", "shadow", "rock", "fire", "water", "grass",
    "electric", "psychic", "rock", "ice", "shadow", "shadow",
    "psychic", "grass", "water", "electric", "shadow", "normal",
    "fire", "water", "grass", "electric", "psychic", "dragon",
    "ice", "fairy",
    "fire", "water", "grass", "electric", "psychic", "rock",
    "ice", "shadow", "normal", "dragon", "fairy", "fire",
    "water", "grass", "electric", "fairy",
]
# Fixed hue (0..360) per type.
TYPE_HUES = {
    "fire": 10, "water": 205, "grass": 120, "electric": 52, "psychic": 285,
    "rock": 30, "ice": 190, "shadow": 265, "normal": 45,
    "dragon": 250, "fairy": 320,
}
# Evolution stages: reach stage i once promptCount >= STAGE_THRESHOLDS[i].
STAGE_THRESHOLDS = [0, 3, 12, 30, 70]
STAGE_NAMES = ["Egg", "Hatchling", "Juvenile", "Adult", "Apex"]


def species_seed(species):
    """Stable 24-bit sprite seed derived from the SPECIES index (not the session),
    so every session of a species shares one sprite pattern."""
    h = hashlib.sha256(("nymonster:species:%d" % int(species)).encode("utf-8"))
    return int(h.hexdigest(), 16) & 0xFFFFFF


# --------------------------------------------------------------------------- #
# Local state files: atomic writes + corruption-tolerant reads
#
# Every small JSON state file (config.json, sessions-meta.json, the Arena link)
# is written via a temp file in the same folder (0600 from birth), fsynced, then
# os.replace()d over the target -- a crash or full disk mid-write leaves the old
# file intact instead of a truncated one. arena.py keeps identical copies of
# these three helpers (it cannot import dashboard without a cycle).
# --------------------------------------------------------------------------- #

def _atomic_write_text(path, text, mode=0o600):
    """Write `text` to `path` atomically with permissions `mode`. Raises OSError
    on failure (the original file is left untouched). A symlinked `path` is
    resolved first, so the real file is replaced and the link is kept."""
    path = os.path.realpath(path)
    d = os.path.dirname(path)
    fd, tmp = tempfile.mkstemp(dir=d, prefix="." + os.path.basename(path) + "-",
                               suffix=".tmp")
    try:
        os.fchmod(fd, mode)
        with os.fdopen(fd, "w", encoding="utf-8") as f:
            fd = None
            f.write(text)
            f.flush()
            os.fsync(f.fileno())
        os.replace(tmp, path)
    except BaseException:
        if fd is not None:
            try:
                os.close(fd)
            except OSError:
                pass
        try:
            os.unlink(tmp)
        except OSError:
            pass
        raise


def _atomic_write_json(path, data, mode=0o600):
    """json.dump `data` to `path` atomically (see _atomic_write_text)."""
    _atomic_write_text(path, json.dumps(data, indent=2), mode=mode)


def _load_json_guarded(path, default=None):
    """Parsed JSON from `path`, or `default` if it is missing/unreadable/corrupt.
    A file that exists but does not parse is renamed to <name>.corrupt-<ts> so
    the next save does not silently destroy it. Permission and other I/O errors
    never quarantine (the file may be fine; we just cannot read it right now)."""
    try:
        with open(path, "rb") as f:
            raw = f.read()
            seen = os.fstat(f.fileno())
    except OSError:
        return default
    try:
        return json.loads(raw.decode("utf-8"))
    except ValueError:  # JSONDecodeError and UnicodeDecodeError
        # Only quarantine the very file we read: a concurrent atomic save may
        # already have swapped a good file in, which must not be moved aside.
        try:
            now = os.stat(path)
        except OSError:
            return default
        if (now.st_ino, now.st_dev, now.st_size, now.st_mtime_ns) != \
                (seen.st_ino, seen.st_dev, seen.st_size, seen.st_mtime_ns):
            return default
        dest = "%s.corrupt-%d" % (path, int(time.time() * 1000))
        try:
            os.replace(path, dest)
        except OSError:
            pass
        return default


DEX_SEED_PATH = os.path.join(HERE, ".dex-seed")
_dex_salt_cache = None


def dex_salt():
    """A stable, per-install random salt for the shiny roll. Generated once and
    persisted, so it never changes on this machine (determinism: a user never
    sees a creature flip shiny), but it differs between installs — so two people
    sharing an Arena do NOT get the identical set of shiny species. Not secret;
    it only seeds a cosmetic roll."""
    global _dex_salt_cache
    if _dex_salt_cache is not None:
        return _dex_salt_cache
    try:
        with open(DEX_SEED_PATH, "r", encoding="utf-8") as f:
            s = f.read().strip()
    except Exception:
        s = ""
    if not re.fullmatch(r"[0-9a-f]{8,64}", s or ""):
        s = secrets.token_hex(8)
        try:
            _atomic_write_text(DEX_SEED_PATH, s)
        except OSError:
            pass
    _dex_salt_cache = s
    return s


def shiny_for_species(species, salt=None):
    """Shiny is a stable PER-SPECIES property (1 in 4 species, ~12 of 48), so every
    session of a shiny species is shiny — the Pokédex and the live cards always
    agree. The per-install `salt` (see dex_salt) keeps that consistency and
    determinism while making the shiny set unique to this machine, so Arena
    friends don't all share the same shinies. Species that passed the original
    1-in-6 roll stay shiny; the rest get a second 1-in-10 roll, which makes the
    total exactly 1/6 + 5/6 * 1/10 = 1/4."""
    salt = dex_salt() if salt is None else salt
    sp = int(species) % 48
    h = int(hashlib.sha256(("nymonster:shiny:%s:%d" % (salt, sp)).encode("utf-8")).hexdigest(), 16)
    if h % 6 == 0:
        return True
    h2 = int(hashlib.sha256(("nymonster:shiny2:%s:%d" % (salt, sp)).encode("utf-8")).hexdigest(), 16)
    return h2 % 10 == 0


def stage_for(prompt_count):
    """Return (stage 0..4, stageName, stagePct 0..1) from a promptCount using
    STAGE_THRESHOLDS. stagePct is progress from this stage's threshold to the
    next (1.0 at the max stage)."""
    pc = int(prompt_count or 0)
    stage = 0
    for i, th in enumerate(STAGE_THRESHOLDS):
        if pc >= th:
            stage = i
    if stage >= len(STAGE_THRESHOLDS) - 1:
        pct = 1.0
    else:
        lo = STAGE_THRESHOLDS[stage]
        hi = STAGE_THRESHOLDS[stage + 1]
        pct = (pc - lo) / (hi - lo) if hi > lo else 1.0
        pct = max(0.0, min(1.0, pct))
    return stage, STAGE_NAMES[stage], round(float(pct), 4)

# Rank titles by level band.
RANKS = [
    (1, "Prompt Apprentice"),
    (3, "Prompt Adept"),
    (5, "Prompt Conjurer"),
    (8, "Prompt Sorcerer"),
    (12, "Prompt Archmage"),
    (18, "Prompt Ascendant"),
    (999, "Prompt Deity"),
]

# Prefixes that mean a "user" record is NOT a real human prompt.
_NON_HUMAN_PREFIXES = (
    "<command-",
    "<local-command",
    "<system-reminder",
    "Caveat:",
    "This session is being continued",
    "[Request interrupted",  # Esc marker: "...by user]", "...by user for tool use]"
)

_PASTED_RE = re.compile(r"<pasted_content\b[^>]*>.*?</pasted_content>", re.DOTALL)
_ARTIFACT_RE = re.compile(r"https://claude\.ai/[^\s)\"']*artifact[^\s)\"']*")
_PR_RE = re.compile(r"https://github\.com/[^\s)\"']+/pull/(\d+)")


# --------------------------------------------------------------------------- #
# Small helpers
# --------------------------------------------------------------------------- #

def now_utc():
    return datetime.now(timezone.utc)


def parse_ts(s):
    """Parse an ISO8601 timestamp to an aware UTC datetime, or None."""
    if not s or not isinstance(s, str):
        return None
    try:
        t = s.strip()
        if t.endswith("Z"):
            t = t[:-1] + "+00:00"
        dt = datetime.fromisoformat(t)
        if dt.tzinfo is None:
            dt = dt.replace(tzinfo=timezone.utc)
        return dt.astimezone(timezone.utc)
    except Exception:
        return None


def strip_markdown(text):
    """Reduce markdown to readable plain text."""
    if not text:
        return ""
    t = text
    # links [label](url) -> label
    t = re.sub(r"\[([^\]]+)\]\([^)]*\)", r"\1", t)
    # images ![alt](url) -> alt
    t = re.sub(r"!\[([^\]]*)\]\([^)]*\)", r"\1", t)
    # code fences / inline backticks
    t = t.replace("```", " ")
    t = t.replace("`", "")
    # headings / list markers / emphasis at token level
    t = re.sub(r"^\s{0,3}#{1,6}\s*", "", t, flags=re.MULTILINE)
    t = re.sub(r"^\s{0,3}[-*+]\s+", "", t, flags=re.MULTILINE)
    t = re.sub(r"\*\*([^*]+)\*\*", r"\1", t)
    t = re.sub(r"\*([^*]+)\*", r"\1", t)
    t = re.sub(r"__([^_]+)__", r"\1", t)
    t = t.replace(">", "")
    # collapse whitespace
    t = re.sub(r"\s+", " ", t).strip()
    return t


def _human_text(content):
    """Human-authored text from a user message's content. Claude Code stores a plain
    text prompt as a string, but a prompt that carries an IMAGE/attachment arrives as
    a list of content blocks (text + image). Pull the text out of either shape; ignore
    tool_result / tool_use / image blocks. Returns "" when there is no human text."""
    if isinstance(content, str):
        return content
    if isinstance(content, list):
        parts = []
        for b in content:
            if isinstance(b, str):
                parts.append(b)
            elif isinstance(b, dict) and b.get("type") == "text":
                parts.append(b.get("text") or "")
        return " ".join(p for p in parts if p).strip()
    return ""


def clean_prompt(content):
    """Strip pasted-content noise and normalise whitespace for a human prompt.
    Accepts a raw string OR a content-block list (image+text prompts)."""
    text = content if isinstance(content, str) else _human_text(content)
    if not text:
        return ""
    t = _PASTED_RE.sub(" [pasted content] ", text)
    t = re.sub(r"\s+", " ", t).strip()
    return t


def is_real_human_prompt(content):
    """A real human prompt: a genuine user turn whose text doesn't start with a noise
    prefix. Accepts a plain string OR a content-block list, so image/attachment prompts
    (which arrive as a list) are counted too. tool_result-only carriers yield no text
    and are ignored."""
    s = _human_text(content).lstrip()
    if not s:
        return False
    for p in _NON_HUMAN_PREFIXES:
        if s.startswith(p):
            return False
    return True


def _is_interrupt_marker(content):
    """True for the "[Request interrupted by user...]" note Claude Code writes when
    the user presses Esc: it ends the turn but is not a prompt."""
    return _human_text(content).lstrip().startswith("[Request interrupted")


# The live payload (sent every stream frame) carries trimmed text; the drawer
# reads the full text from /api/session/<sid> and exports rehydrate it.
LIVE_PROMPT_MAX = 280
LIVE_REPLY_MAX = 400
STREAM_HEARTBEAT_SECS = 15.0


def truncate(s, n):
    if s is None:
        return ""
    s = str(s)
    return s if len(s) <= n else s[: n - 1] + "…"


def _session_hash(session_id):
    """The one canonical hash for a session id. species/shiny (and the pokedex
    mapping) all derive from THIS integer:
        h = int(sha256(sessionId).hexdigest(), 16)
        species = h % 48 ; shiny = (h // 48) % 16 == 0"""
    return int(hashlib.sha256((session_id or "").encode("utf-8")).hexdigest(), 16)


def creature_for(session_id, prompt_count=0):
    """Stable creature for a session. Keeps the legacy emoji/name/hue fields and
    ADDS the original-monster fields (species/type/seed/stage/shiny/...)."""
    h = _session_hash(session_id)
    emoji, name = CREATURES[h % len(CREATURES)]
    hue = h % 360
    species = h % 48
    shiny = shiny_for_species(species)   # per-species, so live cards == Pokédex
    species_name = SPECIES_NAMES[species]
    stype = SPECIES_TYPES[species]
    stage, stage_name, stage_pct = stage_for(prompt_count)
    return {
        "emoji": emoji, "name": name, "hue": hue,
        "species": int(species),
        "speciesName": species_name,
        "type": stype,
        "seed": int(species_seed(species)),
        "typeHue": int(TYPE_HUES.get(stype, 45)),
        "stage": int(stage),
        "stageName": stage_name,
        "stagePct": stage_pct,
        "shiny": bool(shiny),
    }


def rank_for_level(level):
    for threshold, title in RANKS:
        if level <= threshold:
            return title
    return RANKS[-1][1]


def xp_for_level(level):
    """XP needed to advance FROM the given level to the next."""
    return 400 + 120 * level


def derive_level(total_xp):
    """Walk cumulative thresholds from level 1. Returns (level, xp_into, xp_for)."""
    level = 1
    remaining = total_xp
    while True:
        need = xp_for_level(level)
        if remaining < need:
            return level, int(remaining), int(need)
        remaining -= need
        level += 1
        if level > 999:  # safety
            return level, int(remaining), int(xp_for_level(level))


def tool_label(tool_use):
    """Render a short human label for an in-flight tool_use block."""
    if not isinstance(tool_use, dict):
        return None
    name = tool_use.get("name") or "Tool"
    inp = tool_use.get("input") or {}
    detail = ""
    if isinstance(inp, dict):
        for key in ("description", "command", "file_path", "path", "pattern", "query", "url"):
            v = inp.get(key)
            if isinstance(v, str) and v.strip():
                detail = v.strip()
                break
    if detail:
        return truncate(f"{name}: {detail}", 80)
    return truncate(name, 80)


# --------------------------------------------------------------------------- #
# Transcript locating & parsing
# --------------------------------------------------------------------------- #

def find_transcript(session_id):
    """Locate the JSONL for a session id under Claude or Cursor projects. Only a
    UUID-shaped id is looked up: anything else (glob metacharacters, path
    separators) returns None before it reaches glob."""
    if not isinstance(session_id, str) or not _UUID_RE.fullmatch(session_id):
        return None
    if PROJECTS_DIR:
        matches = glob.glob(os.path.join(PROJECTS_DIR, "*", f"{session_id}.jsonl"))
        if matches:
            return matches[0]
    if CURSOR_PROJECTS_DIR:
        matches = glob.glob(os.path.join(
            CURSOR_PROJECTS_DIR, "*", "agent-transcripts", session_id,
            f"{session_id}.jsonl"))
        if matches:
            return matches[0]
    return None


def _extract_links_from_text(text, links, seen):
    if not text:
        return
    for m in _ARTIFACT_RE.finditer(text):
        url = m.group(0)
        if url not in seen:
            seen.add(url)
            links.append({"type": "artifact", "url": url, "label": "artifact"})
    for m in _PR_RE.finditer(text):
        url = m.group(0)
        if url not in seen:
            seen.add(url)
            links.append({"type": "pr", "url": url, "label": f"PR #{m.group(1)}"})


# --------------------------------------------------------------------------- #
# Cost model (ESTIMATE). Prices per 1,000,000 tokens: (input, output)
# --------------------------------------------------------------------------- #

# Anthropic first-party list prices (USD / 1M tokens), checked 2026-10.
# Ordered most-specific first: the first key found in the lowercased model id
# wins, so "opus-5-5" must precede "opus-5". The trailing bare family names are
# fallbacks for ids not listed (e.g. Opus 4 / 4.1, Haiku 3.5).
_PRICES = (
    ("fable-5-1", (10.0, 50.0)),
    ("mythos-5-1", (10.0, 50.0)),
    ("fable", (10.0, 50.0)),
    ("mythos", (10.0, 50.0)),
    ("opus-5-5", (4.0, 20.0)),
    ("opus-5", (5.0, 25.0)),
    ("opus-4-8", (5.0, 25.0)),
    ("opus-4-7", (5.0, 25.0)),
    ("opus-4-6", (5.0, 25.0)),
    ("opus-4-5", (5.0, 25.0)),
    ("opus", (15.0, 75.0)),
    ("sonnet-5", (2.0, 10.0)),
    ("sonnet", (3.0, 15.0)),
    ("haiku-4", (1.0, 5.0)),
    ("haiku", (0.8, 4.0)),
)
_DEFAULT_PRICE = (3.0, 15.0)  # unknown / empty -> Sonnet 4.x pricing

# Cache reads are 0.1x input, except where a model prices them lower.
_CACHE_READ_MULT = (
    ("fable-5-1", 0.025),
    ("mythos-5-1", 0.025),
    ("opus-5-5", 0.05),
)
_CACHE_WRITE_5M = 1.25  # 5-minute TTL write (also the undifferentiated default)
_CACHE_WRITE_1H = 2.0   # 1-hour TTL write


def _price_for(model):
    """Return (input_price, output_price) per 1e6 tokens for a model string."""
    m = (model or "").lower()
    for key, price in _PRICES:
        if key in m:
            return price
    return _DEFAULT_PRICE


def _cache_read_mult(model):
    """Cache-read price as a fraction of the model's input price."""
    m = (model or "").lower()
    for key, mult in _CACHE_READ_MULT:
        if key in m:
            return mult
    return 0.1


def _usage_cost(model, usage):
    """Return (est_cost_usd, output, input, cache_read, cache_creation) for one record.

    Cache writes are priced by TTL when the usage carries the
    cache_creation.ephemeral_5m/1h_input_tokens breakdown (5m at 1.25x input,
    1h at 2x); any undifferentiated remainder is priced at 1.25x."""
    if not isinstance(usage, dict):
        return 0.0, 0, 0, 0, 0
    in_price, out_price = _price_for(model)

    def num(d, k):
        try:
            return max(0, int(d.get(k) or 0))
        except (TypeError, ValueError):
            return 0

    it = num(usage, "input_tokens")
    ot = num(usage, "output_tokens")
    cr = num(usage, "cache_read_input_tokens")
    cc = num(usage, "cache_creation_input_tokens")
    brk = usage.get("cache_creation")
    c5 = c1h = 0
    if isinstance(brk, dict):
        c5 = num(brk, "ephemeral_5m_input_tokens")
        c1h = num(brk, "ephemeral_1h_input_tokens")
    cc = max(cc, c5 + c1h)
    rest = cc - c5 - c1h
    cost = (
        it * in_price
        + cr * in_price * _cache_read_mult(model)
        + (c5 + rest) * in_price * _CACHE_WRITE_5M
        + c1h * in_price * _CACHE_WRITE_1H
        + ot * out_price
    ) / 1_000_000.0
    return cost, ot, it, cr, cc


# Error signatures that mean a session needs attention (case-insensitive).
# Matched only against records that already ARE API errors (see
# _record_error_sig); tool_result text such as a user's permission denial is
# never scanned, so it has no signature here.
_ERROR_SIGS = (
    "organization has disabled", "disabled claude", "rate limit", "overloaded",
    "invalid api key", "credit balance", "billing", "quota", "insufficient",
    "authentication_error", "spend limit", "login expired", "/login",
    "prompt is too long",
)

# Signature for a flagged API-error record whose text matches nothing above:
# the flag alone already means the session needs attention.
_GENERIC_ERROR_SIG = "api_error"


def _match_error(text):
    if not text:
        return None
    low = text.lower()
    for sig in _ERROR_SIGS:
        if sig in low:
            return sig
    return None


def _record_error_sig(o):
    """Error signature for a transcript record that IS an API error, else None.

    Only two record shapes qualify: a system record with subtype "api_error"
    (skipped while Claude Code is still retrying it), and an assistant record
    Claude Code flagged isApiErrorMessage (the synthetic error reply). Ordinary
    assistant prose or system notes that merely mention "billing" or "rate
    limit" never count. A qualifying record whose text matches no known
    signature still returns the generic "api_error" signature."""
    if not isinstance(o, dict):
        return None
    typ = o.get("type")
    parts = []
    if typ == "system":
        if o.get("subtype") != "api_error":
            return None
        ra, mr = o.get("retryAttempt"), o.get("maxRetries")
        if (isinstance(ra, int) and isinstance(mr, int) and not isinstance(ra, bool)
                and not isinstance(mr, bool) and ra < mr):
            return None  # a retry in progress; the final failure is recorded separately
        content = o.get("content")
        if isinstance(content, str):
            parts.append(content)
        err = o.get("error")
        if err is not None:
            parts.append(err if isinstance(err, str) else json.dumps(err, default=str))
    elif typ == "assistant":
        if o.get("isApiErrorMessage") is not True:
            return None
        parts.append(_human_text((o.get("message") or {}).get("content")))
        err = o.get("error")
        if isinstance(err, str):
            parts.append(err)
    else:
        return None
    return _match_error(" ".join(p for p in parts if p)) or _GENERIC_ERROR_SIG


# --------------------------------------------------------------------------- #
# Single-pass per-file scan, with an mtime/size cache.
#
# scan_file(path) returns a rich per-file aggregate that feeds BOTH the season
# scan and the per-session views in ONE pass. A file is re-read only when its
# (mtime, size) changed; otherwise the cached aggregate is reused.
# --------------------------------------------------------------------------- #

_scan_cache = {}
_scan_lock = threading.Lock()


def _new_day():
    return {
        "prompts": 0, "tools": 0, "artifacts": 0, "replies": 0,
        "tools_by_name": {}, "hours": {},
        "output": 0, "input": 0, "cacheRead": 0, "cacheCreation": 0, "cost": 0.0,
    }


def scan_file(path):
    """Return the cached per-file aggregate, re-reading only if mtime/size changed."""
    if not path:
        return None
    try:
        st = os.stat(path)
    except Exception:
        return None
    key = (st.st_mtime, st.st_size)
    with _scan_lock:
        cached = _scan_cache.get(path)
        if cached is not None and cached.get("_key") == key:
            return cached
    if _is_cursor_transcript(path):
        agg = _scan_cursor_uncached(path)
    else:
        agg = _scan_file_uncached(path)
    if agg is not None:
        agg["_key"] = key
        with _scan_lock:
            _scan_cache[path] = agg
    return agg


def _scan_file_uncached(path):
    """Read one transcript once, producing everything downstream views need."""
    agg = {
        "ai_title": None, "last_prompt": None, "last_reply": None, "now_label": None,
        "first_prompt": None, "prompt_count": 0, "last_activity": None, "links": [],
        "folder": os.path.basename(os.path.dirname(path)),
        "per_day": {}, "activity_ts": [], "errors": [], "timeline": [], "files": {},
        "model": "", "tok_output": 0, "tok_input": 0, "tok_cacheRead": 0,
        "tok_cacheCreation": 0, "cost": 0.0,
        # creature fatigue inputs: merged busy spans + oldest still-open tool
        "busy_spans": [], "open_tool_since": None,
    }
    try:
        f = open(path, "r", encoding="utf-8", errors="replace")
    except Exception:
        return agg

    seen_links = set()
    seen_usage = set()  # message.id / requestId whose usage was already counted
    last_assistant_text = None
    last_assistant_tool = None
    busy = []        # (start, end) epoch secs when Claude or the user was at it
    open_tools = {}  # tool_use id -> epoch secs, until its tool_result arrives
    open_tool_names = {}  # tool_use id -> tool name, for the permission-wait hint
    pending_ask = {}  # AskUserQuestion / ExitPlanMode tool_use id -> (kind, epoch secs, text)
    perm_mode = None  # last permissionMode seen on a user record
    turn_floor = None  # start of the current turn: a turn_duration never reaches back before it
    turn_ended = True  # the next user record (even an isMeta one) opens a turn
    last_busy = None   # latest busy instant so far (a running max: records can be out of order)
    rest_until = None  # first busy instant after a stretch the page showed as rest
    ws_state = worksignals.new_state()  # HQ 2.5 work signals: counts only, see worksignals.py

    def mark_busy(t):
        """A busy instant at t. When nothing covered the stretch before it for
        more than FATIGUE_HOLD_SECS (no record, and no open tool inside its
        FATIGUE_MAX_PAIR_SECS horizon), the page showed that stretch as rest,
        so no turn_duration may later reach back across it. This holds however
        the turn was opened: an isMeta message after a slash command or an Esc
        interrupt, a <task-notification>, or a tool that ran past the cap."""
        nonlocal last_busy, rest_until
        if last_busy is None or t > last_busy:
            if last_busy is not None and t - last_busy > FATIGUE_HOLD_SECS:
                cover = last_busy
                if open_tools:
                    cover = max(cover, max(open_tools.values()) + FATIGUE_MAX_PAIR_SECS)
                if t - cover > FATIGUE_HOLD_SECS:
                    rest_until = t
            last_busy = t
        busy.append((t, t))

    with f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            if not isinstance(o, dict):
                continue
            worksignals.observe(o, agg, ws_state)
            try:
                typ = o.get("type")
                ts = parse_ts(o.get("timestamp"))
                diso = ts.date().isoformat() if ts else None
                lhour = ts.astimezone().hour if ts else None
                t = ts.timestamp() if ts else None

                if typ == "ai-title":
                    at = o.get("aiTitle")
                    if at:
                        agg["ai_title"] = at

                elif typ == "last-prompt":
                    lp = o.get("lastPrompt")
                    if lp:
                        agg["last_prompt"] = clean_prompt(lp)

                elif typ == "user":
                    content = (o.get("message") or {}).get("content")
                    # Fatigue: every user record is a busy instant, and a
                    # tool_result closes its tool's interval (capped per pair).
                    if t is not None:
                        mark_busy(t)
                        if isinstance(content, list):
                            for blk in content:
                                if not isinstance(blk, dict) or blk.get("type") != "tool_result":
                                    continue
                                tid = blk.get("tool_use_id")
                                if isinstance(tid, str):
                                    pending_ask.pop(tid, None)
                                    open_tool_names.pop(tid, None)
                                u = open_tools.pop(tid, None) if isinstance(tid, str) else None
                                if u is not None and t >= u:
                                    busy.append((u, min(t, u + FATIGUE_MAX_PAIR_SECS)))
                        # A turn starts at the first user record after the last
                        # one ended (an isMeta agent message can open it) or at
                        # a later prompt / <task-notification>. isMeta inserts
                        # inside a turn (skill body, image note) don't move it.
                        if turn_ended or (not o.get("isMeta") and _opens_turn(content)):
                            turn_floor = t
                            turn_ended = False
                    pm = o.get("permissionMode")
                    if isinstance(pm, str):
                        perm_mode = pm
                    if _is_interrupt_marker(content):
                        open_tools.clear()  # Esc ends the turn and any orphaned tool
                        open_tool_names.clear()
                        pending_ask.clear()
                    if is_real_human_prompt(content):
                        open_tools.clear()  # a new human turn ends any orphaned tool
                        open_tool_names.clear()
                        pending_ask.clear()
                        cleaned = clean_prompt(content)
                        agg["prompt_count"] += 1
                        if agg["first_prompt"] is None:
                            agg["first_prompt"] = cleaned
                        if ts and (agg["last_activity"] is None or ts > agg["last_activity"]):
                            agg["last_activity"] = ts
                        text = _human_text(content)
                        _extract_links_from_text(text, agg["links"], seen_links)
                        if diso:
                            d = agg["per_day"].setdefault(diso, _new_day())
                            d["prompts"] += 1
                            for _ in _ARTIFACT_RE.finditer(text):
                                d["artifacts"] += 1
                            if lhour is not None:
                                d["hours"][lhour] = d["hours"].get(lhour, 0) + 1
                        if ts is not None:
                            agg["activity_ts"].append(ts.timestamp())
                            agg["timeline"].append({
                                "t": ts.isoformat(), "kind": "you",
                                "text": truncate(cleaned, 200), "tool": None,
                            })

                elif typ == "assistant":
                    if t is not None:
                        mark_busy(t)
                    msg = o.get("message") or {}
                    blocks = msg.get("content")
                    model = msg.get("model") or ""
                    if model:
                        agg["model"] = model
                    sig = _record_error_sig(o)
                    if sig and ts is not None:
                        agg["errors"].append((ts.timestamp(), sig))
                    d = agg["per_day"].setdefault(diso, _new_day()) if diso else None
                    # One API response is written as several records (one per
                    # content block) that all repeat the same usage; count it
                    # once per message.id (else requestId; neither -> count).
                    ukey = msg.get("id") or o.get("requestId")
                    if not isinstance(ukey, str) or not ukey:
                        ukey = None
                    if ukey is None or ukey not in seen_usage:
                        if ukey is not None:
                            seen_usage.add(ukey)
                        cost, ot, it, cr, cc = _usage_cost(model, msg.get("usage") or {})
                        agg["cost"] += cost
                        agg["tok_output"] += ot
                        agg["tok_input"] += it
                        agg["tok_cacheRead"] += cr
                        agg["tok_cacheCreation"] += cc
                        if d is not None:
                            d["cost"] += cost
                            d["output"] += ot
                            d["input"] += it
                            d["cacheRead"] += cr
                            d["cacheCreation"] += cc
                    if isinstance(blocks, list):
                        texts = []
                        last_tool = None
                        for b in blocks:
                            if not isinstance(b, dict):
                                continue
                            bt = b.get("type")
                            if bt == "text":
                                txt = b.get("text") or ""
                                texts.append(txt)
                                _extract_links_from_text(txt, agg["links"], seen_links)
                                if d is not None:
                                    for _ in _ARTIFACT_RE.finditer(txt):
                                        d["artifacts"] += 1
                            elif bt == "tool_use":
                                last_tool = b
                                name = b.get("name") or "Tool"
                                # Human-wait tools never open a busy interval.
                                tuid = b.get("id")
                                if (t is not None and tuid and isinstance(tuid, str)
                                        and name not in FATIGUE_SKIP_TOOLS):
                                    open_tools[tuid] = t
                                    open_tool_names[tuid] = name
                                elif t is not None and tuid and isinstance(tuid, str):
                                    pending_ask[tuid] = (_ask_kind(name), t, _ask_text(b))
                                if d is not None:
                                    d["tools"] += 1
                                    d["tools_by_name"][name] = d["tools_by_name"].get(name, 0) + 1
                                    if lhour is not None:
                                        d["hours"][lhour] = d["hours"].get(lhour, 0) + 1
                                if ts is not None:
                                    agg["activity_ts"].append(ts.timestamp())
                                    agg["timeline"].append({
                                        "t": ts.isoformat(), "kind": "tool",
                                        "text": tool_label(b) or name, "tool": name,
                                    })
                                inp = b.get("input") or {}
                                p = inp.get("file_path") or inp.get("path") if isinstance(inp, dict) else None
                                if isinstance(p, str) and p.strip():
                                    nl = name.lower()
                                    if nl == "write":
                                        action = "write"
                                    elif "edit" in nl:
                                        action = "edit"
                                    elif nl == "read":
                                        action = "read"
                                    else:
                                        action = "other"
                                    fe = agg["files"].setdefault(
                                        p, {"path": p, "action": action, "count": 0})
                                    fe["count"] += 1
                        if texts:
                            last_assistant_text = "\n".join(texts)
                            if d is not None:
                                d["replies"] += 1
                                if lhour is not None:
                                    d["hours"][lhour] = d["hours"].get(lhour, 0) + 1
                            if ts is not None:
                                agg["activity_ts"].append(ts.timestamp())
                                agg["timeline"].append({
                                    "t": ts.isoformat(), "kind": "claude",
                                    "text": truncate(strip_markdown(last_assistant_text), 200),
                                    "tool": None,
                                })
                        if last_tool is not None:
                            last_assistant_tool = last_tool
                    if ts and (agg["last_activity"] is None or ts > agg["last_activity"]):
                        agg["last_activity"] = ts

                elif typ == "system":
                    content = o.get("content")
                    sig = _record_error_sig(o)
                    if sig and ts is not None:
                        agg["errors"].append((ts.timestamp(), sig))
                    if isinstance(content, str):
                        if "claude.ai" in content or "github.com" in content:
                            _extract_links_from_text(content, agg["links"], seen_links)
                    # Fatigue: a finished turn covers its own duration (clamped),
                    # but never reaches back past the start of the turn it
                    # closes, nor across a stretch the page already showed as
                    # rest (mark_busy). A turn opened by a <task-notification>
                    # or another session's message can report a durationMs
                    # from an EARLIER turn's start, and a foreground tool that
                    # ran past the pair cap is inside its own turn's duration;
                    # taken at face value either would turn hours the page
                    # showed as rest into work in one refresh.
                    # away_summary and other subtypes are ignored.
                    st = o.get("subtype")
                    dms = o.get("durationMs")
                    if t is not None:
                        if (st == "turn_duration" and isinstance(dms, (int, float))
                                and not isinstance(dms, bool) and dms > 0):
                            mark_busy(t)
                            start = t - min(dms / 1000.0, FATIGUE_MAX_TURN_SECS)
                            for floor in (turn_floor, rest_until):
                                if floor is not None and floor <= t:
                                    start = max(start, floor)
                            busy.append((start, t))
                            turn_floor, turn_ended = t, True
                        elif st in ("local_command", "api_error", "compact_boundary"):
                            mark_busy(t)

                else:
                    if "claude.ai" in line or "github.com" in line:
                        _extract_links_from_text(line, agg["links"], seen_links)
            except Exception:
                # never crash on a single record
                continue

    worksignals.finish(agg, ws_state)
    if last_assistant_text:
        agg["last_reply"] = strip_markdown(last_assistant_text)
    if last_assistant_tool is not None:
        agg["now_label"] = tool_label(last_assistant_tool)
    agg["links"] = agg["links"][:4]
    if len(agg["timeline"]) > 60:
        agg["timeline"] = agg["timeline"][-60:]
    agg["busy_spans"] = _merge_spans(busy, FATIGUE_TAIL_SECS)[-FATIGUE_MAX_SPANS:]
    agg["open_tool_since"] = min(open_tools.values()) if open_tools else None
    if pending_ask:
        kind_, since_, text_ = max(pending_ask.values(), key=lambda v: v[1])
        agg["pending_ask"] = {"kind": kind_, "since": since_, "text": text_}
    else:
        agg["pending_ask"] = None
    gated = [(open_tools[k], open_tool_names.get(k, "")) for k in open_tools
             if _permission_gated(open_tool_names.get(k, ""), perm_mode)]
    agg["gated_tool_open"] = min(gated) if gated else None
    agg["permission_mode"] = perm_mode
    return agg


# Cursor agent transcripts are {role, message:{content:[...]}} lines, not Claude
# Code's {type, timestamp} records. A user turn's clock, when present, is an
# English phrase inside <timestamp>, and the prompt itself is inside <user_query>.
_CURSOR_QUERY_RE = re.compile(r"<user_query>\s*(.*?)\s*</user_query>", re.DOTALL)
_CURSOR_TS_RE = re.compile(r"<timestamp>\s*(.*?)\s*</timestamp>", re.DOTALL)
_CURSOR_CLOCK_RE = re.compile(
    r"([A-Za-z]+)\s+(\d{1,2}),\s*(\d{4}),\s*(\d{1,2}):(\d{2})\s*(AM|PM)\b",
    re.IGNORECASE)
_CURSOR_TZ_RE = re.compile(r"\((?:UTC|GMT)?([+-])(\d{1,2}):(\d{2})\)\s*$")
_CURSOR_MONTHS = {
    "jan": 1, "feb": 2, "mar": 3, "apr": 4, "may": 5, "jun": 6,
    "jul": 7, "aug": 8, "sep": 9, "oct": 10, "nov": 11, "dec": 12,
}
# Injected by Cursor after a background task; not something the user typed.
_CURSOR_SKIP_QUERIES = (
    "briefly inform the user about the task result",
)


def _is_cursor_transcript(path):
    """True for a file under CURSOR_PROJECTS_DIR in an agent-transcripts tree."""
    if not isinstance(path, str) or not path or not CURSOR_PROJECTS_DIR:
        return False
    try:
        root = os.path.realpath(CURSOR_PROJECTS_DIR)
        real = os.path.realpath(path)
    except Exception:
        return False
    if real != root and not real.startswith(root + os.sep):
        return False
    return (os.sep + "agent-transcripts" + os.sep) in (os.sep + os.path.normpath(path))


def _cursor_project_slug(path):
    """The project-dir slug: the folder directly above agent-transcripts."""
    parts = os.path.normpath(path or "").split(os.sep)
    try:
        i = parts.index("agent-transcripts")
    except ValueError:
        return os.path.basename(os.path.dirname(path or ""))
    return parts[i - 1] if i > 0 else ""


def iter_transcript_paths():
    """Claude Code transcripts plus Cursor parent agent sessions.

    Cursor subagents (`.../agent-transcripts/<id>/subagents/<id>.jsonl`) are one
    level deeper than this glob, so they stay out of the session list."""
    if PROJECTS_DIR:
        for p in glob.glob(os.path.join(PROJECTS_DIR, "*", "*.jsonl")):
            yield p
    if CURSOR_PROJECTS_DIR:
        for p in glob.glob(os.path.join(
                CURSOR_PROJECTS_DIR, "*", "agent-transcripts", "*", "*.jsonl")):
            yield p


def _cursor_message_text(o):
    msg = o.get("message") if isinstance(o, dict) else None
    if not isinstance(msg, dict):
        return ""
    return _human_text(msg.get("content"))


def _cursor_user_query(text):
    """The human prompt inside a Cursor user record, or None for system turns."""
    if not text:
        return None
    found = _CURSOR_QUERY_RE.findall(text)
    if not found:
        return None
    q = re.sub(r"\s+", " ", found[-1]).strip()
    if not q:
        return None
    low = q.lower()
    for prefix in _CURSOR_SKIP_QUERIES:
        if low.startswith(prefix):
            return None
    return q


def _parse_cursor_clock(text):
    """<timestamp>Thursday, Sep 24, 2026, 1:44 PM (UTC+5:30)</timestamp> -> UTC.

    Month names are matched in English on purpose: Cursor writes them in
    English regardless of the machine locale, and strptime('%b') would not."""
    m = _CURSOR_TS_RE.search(text or "")
    if not m:
        return None
    raw = re.sub(r"^[A-Za-z]+,\s*", "", m.group(1).strip())
    tz = None
    zm = _CURSOR_TZ_RE.search(raw)
    rest = raw
    if zm:
        rest = raw[:zm.start()].strip()
        sign = 1 if zm.group(1) == "+" else -1
        try:
            tz = timezone(timedelta(hours=sign * int(zm.group(2)),
                                    minutes=sign * int(zm.group(3))))
        except Exception:
            tz = None
    cm = _CURSOR_CLOCK_RE.search(rest)
    if not cm:
        return None
    mon = _CURSOR_MONTHS.get(cm.group(1)[:3].lower())
    if not mon:
        return None
    try:
        hour = int(cm.group(4)) % 12
        if cm.group(6).upper() == "PM":
            hour += 12
        dt = datetime(int(cm.group(3)), mon, int(cm.group(2)),
                      hour, int(cm.group(5)))
    except ValueError:
        return None
    if tz is None:
        tz = datetime.now().astimezone().tzinfo or timezone.utc
    try:
        return dt.replace(tzinfo=tz).astimezone(timezone.utc)
    except Exception:
        return None


def _path_from_project_slug(slug):
    """Best-effort absolute path for a Cursor project slug.

    Cursor replaces every '/' with '-', so a directory name that itself
    contains '-' is recovered by taking the longest existing directory at
    each step. Returns '' when the slug does not resolve to a real directory."""
    if not isinstance(slug, str) or not slug or "/" in slug or "\\" in slug or ".." in slug:
        return ""
    raw = slug[1:] if slug.startswith("-") else slug
    parts = [p for p in raw.split("-") if p]
    if not parts:
        return ""
    cur = os.sep
    i = 0
    while i < len(parts):
        found = None
        nxt = None
        for j in range(len(parts), i, -1):
            name = "-".join(parts[i:j])
            cand = os.path.join(cur, name) if cur != os.sep else (os.sep + name)
            if os.path.isdir(cand):
                found = cand
                nxt = j
                break
        if not found:
            break
        cur = found
        i = nxt
    if i == len(parts) and cur != os.sep and os.path.isdir(cur):
        return cur
    return ""


def _scan_cursor_uncached(path):
    """One Cursor agent transcript, in the same aggregate shape as Claude's."""
    agg = {
        "ai_title": None, "last_prompt": None, "last_reply": None, "now_label": None,
        "first_prompt": None, "prompt_count": 0, "last_activity": None, "links": [],
        "folder": _cursor_project_slug(path),
        "per_day": {}, "activity_ts": [], "errors": [], "timeline": [], "files": {},
        "model": "", "tok_output": 0, "tok_input": 0, "tok_cacheRead": 0,
        "tok_cacheCreation": 0, "cost": 0.0,
        "busy_spans": [], "open_tool_since": None,
        "pending_ask": None, "gated_tool_open": None, "permission_mode": None,
        "source": "cursor",
    }
    try:
        f = open(path, "r", encoding="utf-8", errors="replace")
    except Exception:
        return agg

    seen_links = set()
    last_assistant_text = None
    last_assistant_tool = None
    busy = []
    turn_ts = None
    pending = []

    def apply(ts, kind, text=None, tool=None, file_path=None, file_action=None):
        nonlocal last_assistant_text, last_assistant_tool
        if ts is None:
            return
        t = ts.timestamp()
        busy.append((t, t))
        if agg["last_activity"] is None or ts > agg["last_activity"]:
            agg["last_activity"] = ts
        diso = ts.date().isoformat()
        lhour = ts.astimezone().hour
        d = agg["per_day"].setdefault(diso, _new_day())
        agg["activity_ts"].append(t)
        if kind == "you":
            cleaned = text or ""
            agg["prompt_count"] += 1
            if agg["first_prompt"] is None:
                agg["first_prompt"] = cleaned
            agg["last_prompt"] = cleaned
            _extract_links_from_text(cleaned, agg["links"], seen_links)
            d["prompts"] += 1
            for _ in _ARTIFACT_RE.finditer(cleaned):
                d["artifacts"] += 1
            d["hours"][lhour] = d["hours"].get(lhour, 0) + 1
            agg["timeline"].append({
                "t": ts.isoformat(), "kind": "you",
                "text": truncate(cleaned, 200), "tool": None,
            })
        elif kind == "tool":
            name = tool or "Tool"
            d["tools"] += 1
            d["tools_by_name"][name] = d["tools_by_name"].get(name, 0) + 1
            d["hours"][lhour] = d["hours"].get(lhour, 0) + 1
            label = text or name
            agg["timeline"].append({
                "t": ts.isoformat(), "kind": "tool",
                "text": truncate(label, 200), "tool": name,
            })
            if isinstance(file_path, str) and file_path.strip():
                fe = agg["files"].setdefault(
                    file_path, {"path": file_path, "action": file_action or "other",
                                "count": 0})
                fe["count"] += 1
        elif kind == "reply":
            last_assistant_text = text or ""
            d["replies"] += 1
            d["hours"][lhour] = d["hours"].get(lhour, 0) + 1
            _extract_links_from_text(last_assistant_text, agg["links"], seen_links)
            for _ in _ARTIFACT_RE.finditer(last_assistant_text):
                d["artifacts"] += 1
            agg["timeline"].append({
                "t": ts.isoformat(), "kind": "claude",
                "text": truncate(strip_markdown(last_assistant_text), 200),
                "tool": None,
            })

    def emit(ts, fn):
        nonlocal turn_ts
        if ts is not None and (turn_ts is None or ts >= turn_ts):
            if turn_ts is None and pending:
                queued = pending[:]
                pending.clear()
                turn_ts = ts
                for fn0 in queued:
                    fn0(ts)
            turn_ts = ts
        use = ts or turn_ts
        if use is None:
            pending.append(fn)
        else:
            fn(use)

    with f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            if not isinstance(o, dict):
                continue
            try:
                role = o.get("role")
                if role == "user":
                    text = _cursor_message_text(o)
                    ts = _parse_cursor_clock(text)
                    query = _cursor_user_query(text)
                    if query:
                        emit(ts, lambda t, q=query: apply(t, "you", text=q))
                    elif ts is not None:
                        emit(ts, lambda t: None)
                elif role == "assistant":
                    blocks = (o.get("message") or {}).get("content")
                    if isinstance(blocks, str):
                        blocks = [{"type": "text", "text": blocks}]
                    if not isinstance(blocks, list):
                        continue
                    texts = []
                    for b in blocks:
                        if not isinstance(b, dict):
                            continue
                        bt = b.get("type")
                        if bt == "text":
                            txt = b.get("text") or ""
                            if txt.strip():
                                texts.append(txt)
                        elif bt == "tool_use":
                            if texts:
                                joined = "\n".join(texts)
                                texts = []
                                emit(None, lambda t, j=joined: apply(t, "reply", text=j))
                            name = b.get("name") or "Tool"
                            label = tool_label(b) or name
                            inp = b.get("input") if isinstance(b.get("input"), dict) else {}
                            fp = inp.get("path") or inp.get("file_path") or inp.get("target_file")
                            nl = name.lower()
                            if nl in ("write",):
                                action = "write"
                            elif "edit" in nl or nl in ("strreplace", "delete"):
                                action = "edit"
                            elif nl in ("read", "grep", "glob"):
                                action = "read"
                            else:
                                action = "other"
                            last_assistant_tool = b
                            emit(None, lambda t, lab=label, nm=name, p=fp, act=action:
                                 apply(t, "tool", text=lab, tool=nm,
                                       file_path=p, file_action=act))
                    if texts:
                        joined = "\n".join(texts)
                        emit(None, lambda t, j=joined: apply(t, "reply", text=j))
            except Exception:
                continue

    try:
        mt = datetime.fromtimestamp(os.path.getmtime(path), tz=timezone.utc)
    except Exception:
        mt = None
    if pending and mt is not None:
        queued = pending[:]
        pending.clear()
        for fn0 in queued:
            fn0(mt)
    if mt is not None and (agg["last_activity"] is None or mt > agg["last_activity"]):
        agg["last_activity"] = mt
    if last_assistant_text:
        agg["last_reply"] = strip_markdown(last_assistant_text)
    if last_assistant_tool is not None:
        agg["now_label"] = tool_label(last_assistant_tool)
    if agg["first_prompt"] and not agg["ai_title"]:
        agg["ai_title"] = truncate(agg["first_prompt"], 80)
    agg["links"] = agg["links"][:4]
    if len(agg["timeline"]) > 60:
        agg["timeline"] = agg["timeline"][-60:]
    agg["busy_spans"] = _merge_spans(busy, FATIGUE_TAIL_SECS)[-FATIGUE_MAX_SPANS:]
    return agg


def parse_transcript(path):
    """Back-compat wrapper: same shape as before, now backed by the file cache."""
    agg = scan_file(path)
    if agg is None:
        return {
            "ai_title": None, "last_prompt": None, "last_reply": None, "now_label": None,
            "first_prompt": None, "prompt_count": 0, "last_activity": None, "links": [],
        }
    return {
        "ai_title": agg["ai_title"], "last_prompt": agg["last_prompt"],
        "last_reply": agg["last_reply"], "now_label": agg["now_label"],
        "first_prompt": agg["first_prompt"], "prompt_count": agg["prompt_count"],
        "last_activity": agg["last_activity"], "links": agg["links"][:4],
    }


def _session_tokens(agg):
    """Token/cost summary for a single transcript aggregate."""
    out = agg.get("tok_output", 0)
    inp = agg.get("tok_input", 0)
    cr = agg.get("tok_cacheRead", 0)
    total = out + inp + cr + agg.get("tok_cacheCreation", 0)
    return {
        "output": int(out), "input": int(inp), "cacheRead": int(cr),
        "total": int(total), "estCostUSD": round(agg.get("cost", 0.0), 4),
        "model": agg.get("model", "") or "",
    }


def _buckets_from_ts(activity_ts, n_buckets, span_secs):
    """Bucket activity timestamps over the last span_secs into n_buckets (oldest->newest)."""
    now = now_utc().timestamp()
    start = now - span_secs
    width = span_secs / n_buckets
    buckets = [0] * n_buckets
    for ts in activity_ts:
        if ts < start or ts > now:
            continue
        idx = int((ts - start) / width)
        if idx < 0:
            idx = 0
        elif idx >= n_buckets:
            idx = n_buckets - 1
        buckets[idx] += 1
    return buckets


# --------------------------------------------------------------------------- #
# Creature fatigue (local, cosmetic, deterministic)
#
# A creature tires while its session works and recovers while it rests. Load
# (seconds) rises 1:1 with busy time; the first FATIGUE_TAIL_SECS after the last
# record still count as work, load holds until FATIGUE_HOLD_SECS, then drains
# FATIGUE_REST_RATE times faster than it built. From FATIGUE_RISK_SECS of load
# it may faint on an absolute 15-min tick (a per-session hash roll), and it
# always has by FATIGUE_CERTAIN_SECS. Food from the Arena pantry takes load off.
#
# Nothing here is stored: it is recomputed from the cached busy spans on every
# build. It never feeds XP, the season, the Pokedex or the Arena publish.
# --------------------------------------------------------------------------- #

FATIGUE_WINDOW_SECS = 86400
FATIGUE_TAIL_SECS = 300          # work tail; also the scan merge gap
FATIGUE_HOLD_SECS = 600          # recovery starts this long after the last record
FATIGUE_REST_RATE = 4.0
FATIGUE_TIRED_SECS = 3600
FATIGUE_FATIGUED_SECS = 7200     # also the wake threshold (load < this)
FATIGUE_RISK_SECS = 10800
FATIGUE_CERTAIN_SECS = 14400     # also the energy scale
FATIGUE_CAP_SECS = 18000
FATIGUE_TICK_SECS = 900
FATIGUE_REVIVE_TO_SECS = 6300
FATIGUE_MAX_PAIR_SECS = 3600     # cap on tool pairs and on the live extension
FATIGUE_MAX_TURN_SECS = 21600    # turn_duration clamp
FATIGUE_MAX_SPANS = 96
FATIGUE_SKIP_TOOLS = ("AskUserQuestion", "ExitPlanMode")

# --- "needs you" signals read from the transcript ---------------------------
# A tab waiting on a question or a plan approval shows an AskUserQuestion /
# ExitPlanMode tool_use with no tool_result yet. A tool that needs permission in
# the session's permission mode and has been open a while MAY be waiting on a
# prompt: that is only a hint (likelyAwaiting), never "needs".
ASK_TEXT_MAX = 140
# claude agents --json status values that mean an interactive tab is parked on
# you. Only "busy" and "idle" have been observed so far; the rest are accepted
# defensively so a future status does not read as idle.
LIVE_WAIT_STATES = ("waiting", "blocked", "needs_input", "awaiting_input", "permission")
LIKELY_AWAIT_SECS = 20
_EDIT_TOOLS = ("Edit", "Write", "NotebookEdit", "MultiEdit")
_GATED_TOOLS = ("Bash", "WebFetch") + _EDIT_TOOLS


def _iso_or_none(epoch):
    if not isinstance(epoch, (int, float)):
        return None
    try:
        return datetime.fromtimestamp(epoch, tz=timezone.utc).isoformat()
    except (OverflowError, OSError, ValueError):
        return None


def _ask_kind(name):
    return "plan" if name == "ExitPlanMode" else "question"


def _ask_text(block):
    """First question of an AskUserQuestion call, trimmed; "" for anything else."""
    inp = block.get("input") if isinstance(block, dict) else None
    qs = inp.get("questions") if isinstance(inp, dict) else None
    if isinstance(qs, list) and qs and isinstance(qs[0], dict):
        q = qs[0].get("question")
        if isinstance(q, str):
            return truncate(" ".join(q.split()), ASK_TEXT_MAX)
    return ""


def _permission_gated(name, mode):
    """Would this tool normally show a permission prompt in this mode?"""
    if not name or mode in (None, "auto", "bypassPermissions", "dontAsk"):
        return False
    if name.startswith("mcp__") or name in ("Bash", "WebFetch"):
        return True
    return mode == "default" and name in _EDIT_TOOLS

# kind -> (seconds of load removed, revives). Mirrors CATALOG in backend
# app/pantry.py (restoreMins * 60); tests/test_catalog_sync.py checks it. A
# revive item wakes a fainted creature at FATIGUE_REVIVE_TO_SECS of load, then
# takes its seconds off that.
FOOD_EFFECTS = {"berry": (1200, False), "bread": (1200, False),
                "riceball": (2700, False), "coffee": (2700, False),
                "bento": (7200, False), "noodles": (7200, False),
                "hotpot": (10800, False), "tonic": (0, True), "elixir": (3600, True),
                "strawberry": (1500, False), "dango": (3300, False),
                "omelette": (8100, False), "watermelon": (1500, False),
                "shavedice": (3300, False), "curry": (8100, False),
                "apple": (1500, False), "sweetpotato": (3300, False),
                "pumpkinstew": (8100, False), "chestnuts": (1500, False),
                "cocoa": (3300, False), "oden": (8100, False)}

FATIGUE_RESTED = {"state": "rested", "energy": 1.0, "loadMins": 0, "mayFaint": False,
                  "phase": "resting", "restInMins": 0, "restMins": 0, "streakMins": 0,
                  "lastMeal": None}


def _opens_turn(content):
    """True for the content of a user record that starts a turn: a prompt, a
    slash command or a <task-notification> (any string), or a list with a block
    that isn't a tool_result. A tool_result carrier happens inside a turn."""
    if isinstance(content, str):
        return bool(content.strip())
    if isinstance(content, list):
        return any(isinstance(b, dict) and b.get("type") != "tool_result" for b in content)
    return False


def _merge_spans(intervals, gap):
    """Sort (start, end) pairs and merge those at most `gap` seconds apart.
    Sorting also repairs out-of-order transcript timestamps."""
    out = []
    for s, e in sorted((float(a), float(b)) for a, b in intervals if b >= a):
        if out and s - out[-1][1] <= gap:
            if e > out[-1][1]:
                out[-1][1] = e
        else:
            out.append([s, e])
    return out


def _faint_roll(sid, k):
    """Deterministic [0, 1) roll for tick k (an absolute epoch // 900)."""
    h = hashlib.sha256(("hq:faint:%s:%d" % (sid, k)).encode("utf-8")).hexdigest()
    return int(h[:8], 16) / 4294967296.0


def fatigue_for(sid, spans, now, open_since=None, meals=()):
    """The creature's fatigue at `now` from its busy spans and meals.

    spans: [[start, end], ...] epoch secs, any order. meals: (at, kind) pairs
    for THIS session. open_since: the oldest still-open tool, passed only for a
    live, busy, interactive session so a long tool call keeps tiring it."""
    lo = now - FATIGUE_WINDOW_SECS
    sp = _merge_spans([(max(s, lo), min(e, now)) for s, e in spans
                       if e >= lo and s <= now], FATIGUE_TAIL_SECS)
    # Live extension: an orphan that started before the last span is ignored,
    # and the cap equals the pair cap so the later tool_result changes nothing.
    if open_since is not None and sp and sp[-1][0] <= open_since <= now:
        ext = min(now, open_since + FATIGUE_MAX_PAIR_SECS)
        if ext > sp[-1][1]:
            sp[-1][1] = ext
    ml = sorted((float(t), k) for t, k in meals
                if lo <= t <= now and k in FOOD_EFFECTS)
    # Load is provably 0 past hold + a full drain of the cap: O(1) for idle cards.
    if not ml and (not sp or now - sp[-1][1] >
                   FATIGUE_HOLD_SECS + FATIGUE_CAP_SECS / FATIGUE_REST_RATE):
        return dict(FATIGUE_RESTED)

    # Segments: ("work", a, b, None) or ("rest", a, b, recovery_start).
    segs = []
    for i, (s, e) in enumerate(sp):
        nxt = sp[i + 1][0] if i + 1 < len(sp) else now
        wend = min(e + FATIGUE_TAIL_SECS, nxt)
        segs.append(("work", s, wend, None))
        if nxt > wend:
            segs.append(("rest", wend, nxt, e + FATIGUE_HOLD_SECS))

    load = 0.0
    ko = False
    meal = None

    def work(a, b):
        nonlocal load, ko
        x = a
        while x < b:
            bnd = (math.floor(x / FATIGUE_TICK_SECS) + 1) * FATIGUE_TICK_SECS
            y = min(b, bnd)
            load = min(FATIGUE_CAP_SECS, load + (y - x))
            if y == bnd and not ko and load >= FATIGUE_RISK_SECS:
                p = min(1.0, (load - FATIGUE_RISK_SECS) /
                        float(FATIGUE_CERTAIN_SECS - FATIGUE_RISK_SECS))
                if _faint_roll(sid, int(bnd // FATIGUE_TICK_SECS)) < p:
                    ko = True
            x = y

    def rest(a, b, rec):
        # rec is absolute, so a meal inside a pause does not restart the hold.
        nonlocal load, ko
        d = b - max(a, rec)
        if d > 0:
            load = max(0.0, load - d * FATIGUE_REST_RATE)
        if ko and load < FATIGUE_FATIGUED_SECS:
            ko = False

    def eat(t, k):
        nonlocal load, ko, meal
        credit, revives = FOOD_EFFECTS[k]
        if revives:
            ko = False
            load = max(0.0, min(load, FATIGUE_REVIVE_TO_SECS) - credit)
        else:
            load = max(0.0, load - credit)
            if ko and load < FATIGUE_FATIGUED_SECS:
                ko = False
        meal = (t, k)

    mi = 0
    for kind, a, b, rec in segs:
        while mi < len(ml) and ml[mi][0] < a:
            eat(*ml[mi])
            mi += 1
        cur = a
        while mi < len(ml) and ml[mi][0] < b:   # a meal splits its segment
            t, k = ml[mi]
            if kind == "work":
                work(cur, t)
            else:
                rest(cur, t, rec)
            eat(t, k)
            mi += 1
            cur = t
        if kind == "work":
            work(cur, b)
        else:
            rest(cur, b, rec)
    while mi < len(ml):
        eat(*ml[mi])
        mi += 1

    if ko:
        state = "unconscious"
    elif load >= FATIGUE_FATIGUED_SECS:
        state = "fatigued"
    elif load >= FATIGUE_TIRED_SECS:
        state = "tired"
    else:
        state = "rested"

    last_end = sp[-1][1] if sp else None
    p = (now - last_end) if sp else FATIGUE_HOLD_SECS + 1
    if p <= FATIGUE_TAIL_SECS:
        phase = "active"
    elif p <= FATIGUE_HOLD_SECS:
        phase = "pause"
    else:
        phase = "resting"

    rest_mins = 0
    if state != "rested":
        # time until it wakes (unconscious) or is rested again, with a clean break
        target = FATIGUE_FATIGUED_SECS if ko else FATIGUE_TIRED_SECS
        tail_left = max(0.0, FATIGUE_TAIL_SECS - p)
        hold_left = max(0.0, FATIGUE_HOLD_SECS - p)
        need = max(0.0, load + tail_left - target + 1) / FATIGUE_REST_RATE
        rest_mins = int(math.ceil((hold_left + need) / 60.0))

    streak = 0
    if sp and phase != "resting":
        run_start = sp[-1][0]
        for i in range(len(sp) - 2, -1, -1):
            if sp[i + 1][0] - sp[i][1] <= FATIGUE_HOLD_SECS:
                run_start = sp[i][0]
            else:
                break
        streak = int((min(now, last_end + FATIGUE_TAIL_SECS) - run_start) // 60)

    return {
        "state": state,
        "energy": round(max(0.0, min(1.0, 1.0 - load / FATIGUE_CERTAIN_SECS)), 3),
        "loadMins": int(load // 60),
        "mayFaint": (not ko) and load >= FATIGUE_RISK_SECS,
        "phase": phase,
        "restInMins": 0 if phase == "resting"
        else int(math.ceil((FATIGUE_HOLD_SECS - p) / 60.0)),
        "restMins": rest_mins,
        "streakMins": streak,
        "lastMeal": ({"kind": meal[1],
                      "at": datetime.fromtimestamp(meal[0], tz=timezone.utc).isoformat()}
                     if meal else None),
    }


def _fatigue_safe(sid, agg, open_since, meals, now=None):
    """fatigue_for over a scan aggregate; one bad transcript never breaks the payload."""
    try:
        return fatigue_for(sid, (agg or {}).get("busy_spans") or [],
                           now or time.time(), open_since, meals)
    except Exception:
        return dict(FATIGUE_RESTED)


def _offpayload_fatigue(sid, agg):
    """creature.fatigue for a transcript that isn't in the payload (a drawer
    opened from search or a project: past the archive cap, or no real prompt),
    or None when creature energy is off. The session detail shows it and the
    eat gate checks it, so both always agree."""
    try:
        fz_on = bool(load_config().get("creatureFatigue", True))
    except Exception:
        fz_on = True
    if not fz_on:
        return None
    try:
        sid_meals = load_meals().get(sid, ())
    except Exception:
        sid_meals = ()
    return _fatigue_safe(sid, agg, None, sid_meals)


def session_fatigue_now(sess):
    """Fresh fatigue for the session a snack was for (the eat response's meter)."""
    sid = sess.get("sessionId") or ""
    agg = scan_file(find_transcript(sid)) or {}
    ext = agg.get("open_tool_since") if (sess.get("kind") == "interactive"
                                         and sess.get("rawStatus") == "busy") else None
    return _fatigue_safe(sid, agg, ext, load_meals().get(sid, ()))


# --------------------------------------------------------------------------- #
# Season stats (30-day scan of ALL transcripts)
# --------------------------------------------------------------------------- #

# A flexible streak: it survives missed days as long as no 7-day stretch inside it
# has more than 2 of them (at least 5 active days in every 7). Today never counts
# as a miss while it is still in progress. The streak runs from its oldest to its
# newest active day, so a single quiet day no longer resets it to zero.
STREAK_WINDOW = 7
STREAK_MAX_MISSES = 2


def flex_streak(active, end, lookback=400):
    """Length in days of the flexible streak ending at `end` (a date)."""
    start = end if end in active else end - timedelta(days=1)
    span = []          # True/False per day, newest first
    d = start
    for _ in range(lookback):
        span.append(d in active)
        if span[-STREAK_WINDOW:].count(False) > STREAK_MAX_MISSES:
            span.pop()
            break
        d -= timedelta(days=1)
    if True not in span:
        return 0
    newest = span.index(True)
    oldest = len(span) - 1 - span[::-1].index(True)
    return oldest - newest + 1


def compute_season():
    """
    Scan all *.jsonl under projects, counting per-day real prompts, tool_use blocks,
    and artifact links over the last 30 days. Build XP / level / streak / calendar /
    achievements / totals.
    """
    today = now_utc().date()
    window_start = today - timedelta(days=29)  # 30-day inclusive window

    # per-day tallies
    day_prompts = {}   # date -> count
    day_tools = {}     # date -> count
    day_artifacts = {} # date -> count
    day_replies = {}   # date -> count (assistant records)
    day_cost = {}      # date -> est USD
    active_dates = set()
    folders = set()

    # extended insights (30-day window)
    tok_out = tok_in = tok_cr = tok_cc = 0
    total_cost = 0.0
    tool_counts = {}
    folder_prompts = {}
    folder_tools = {}
    hourly = [0] * 24
    night_owl = False

    files = list(iter_transcript_paths())
    for path in files:
        agg = scan_file(path)
        if agg is None:
            continue
        fol = agg.get("folder") or ""
        folders.add(fol)
        for diso, dd in agg.get("per_day", {}).items():
            try:
                d = date.fromisoformat(diso)
            except Exception:
                continue
            if d < window_start or d > today:
                continue
            p = dd["prompts"]; t = dd["tools"]; a = dd["artifacts"]; r = dd["replies"]
            if p:
                day_prompts[d] = day_prompts.get(d, 0) + p
                active_dates.add(d)
                folder_prompts[fol] = folder_prompts.get(fol, 0) + p
            if t:
                day_tools[d] = day_tools.get(d, 0) + t
                folder_tools[fol] = folder_tools.get(fol, 0) + t
            if a:
                day_artifacts[d] = day_artifacts.get(d, 0) + a
            if r:
                day_replies[d] = day_replies.get(d, 0) + r
                active_dates.add(d)
            day_cost[d] = day_cost.get(d, 0.0) + dd["cost"]
            tok_out += dd["output"]; tok_in += dd["input"]
            tok_cr += dd["cacheRead"]; tok_cc += dd["cacheCreation"]
            total_cost += dd["cost"]
            for nm, c in dd["tools_by_name"].items():
                tool_counts[nm] = tool_counts.get(nm, 0) + c
            for h, c in dd["hours"].items():
                if 0 <= h < 24:
                    hourly[h] += c
                    if h <= 5 and c > 0:
                        night_owl = True

    total_prompts = sum(day_prompts.values())
    total_tools = sum(day_tools.values())
    total_artifacts = sum(day_artifacts.values())
    active_days = len(active_dates)

    xp = total_prompts * 10 + total_tools * 3 + total_artifacts * 40
    level, xp_into, xp_for = derive_level(xp)
    pct = round((xp_into / xp_for) * 100.0, 1) if xp_for else 0.0

    # streak: flexible (see flex_streak) -- at least 5 active days in every 7
    streak = flex_streak(active_dates, today)
    # best streak within the window: the longest flexible streak ending on any day
    best_streak = max([streak] + [flex_streak(active_dates, d) for d in active_dates])

    # calendar: last 14 days oldest -> newest
    calendar = []
    counts_for_heat = []
    for i in range(13, -1, -1):
        d = today - timedelta(days=i)
        c = day_prompts.get(d, 0) + day_replies.get(d, 0)
        counts_for_heat.append(c)
    maxc = max(counts_for_heat) if counts_for_heat else 0
    for idx, i in enumerate(range(13, -1, -1)):
        d = today - timedelta(days=i)
        c = counts_for_heat[idx]
        if c <= 0:
            heat = 0
        elif maxc <= 0:
            heat = 0
        else:
            frac = c / maxc
            if frac >= 0.75:
                heat = 4
            elif frac >= 0.5:
                heat = 3
            elif frac >= 0.25:
                heat = 2
            else:
                heat = 1
        calendar.append({
            "date": d.isoformat(),
            "count": c,
            "heat": heat,
            "today": d == today,
        })

    # night owl: computed in the single pass above (any local-hour 0..5 activity).

    distinct_folders = len(folders)

    achievements = [
        _ach("century", "Century", "100+ prompts in 30 days", "💯",
             total_prompts, 100),
        _ach("artificer", "Artificer", "10+ artifacts in 30 days", "🎨",
             total_artifacts, 10),
        _ach("streak_keeper", "Streak Keeper", "5+ day streak", "🔥",
             streak, 5),
        _ach("tool_smith", "Tool Smith", "500+ tool calls in 30 days", "🛠️",
             total_tools, 500),
        _ach("polyglot", "Polyglot", "Worked in 5+ distinct folders", "🌐",
             distinct_folders, 5),
        _ach("marathon", "Marathoner", "Active on 20+ days", "🏃",
             active_days, 20),
        {
            "id": "night_owl", "name": "Night Owl",
            "desc": "Coded between midnight and 5am",
            "icon": "🦉",
            "unlocked": bool(night_owl),
            "progress": 1.0 if night_owl else 0.0,
        },
        _ach("power_user", "Power User", "1000+ XP in 30 days", "⚡",
             xp, 1000),
    ]

    # --- extended insights (all additive) ---
    tokens = {
        "output": int(tok_out), "input": int(tok_in), "cacheRead": int(tok_cr),
        "total": int(tok_out + tok_in + tok_cr + tok_cc),
        "estCostUSD": round(total_cost, 2),
    }
    tool_breakdown = sorted(
        ({"name": nm, "count": int(c)} for nm, c in tool_counts.items()),
        key=lambda x: -x["count"],
    )[:8]
    fl = []
    for f2 in set(list(folder_prompts) + list(folder_tools)):
        pp = folder_prompts.get(f2, 0)
        tt = folder_tools.get(f2, 0)
        fl.append({"folder": f2, "prompts": int(pp), "tools": int(tt),
                   "score": int(pp * 10 + tt)})
    fl.sort(key=lambda x: -x["score"])
    folder_leaderboard = fl[:6]
    daily_cost = []
    for i in range(13, -1, -1):
        dd2 = today - timedelta(days=i)
        daily_cost.append({"date": dd2.isoformat(), "usd": round(day_cost.get(dd2, 0.0), 4)})

    return {
        "level": level,
        "xp": int(xp),
        "xpIntoLevel": xp_into,
        "xpForLevel": xp_for,
        "pct": pct,
        "rank": rank_for_level(level),
        "streak": int(streak),
        "bestStreak": int(best_streak),
        "totals": {
            "prompts": int(total_prompts),
            "tools": int(total_tools),
            "artifacts": int(total_artifacts),
            "activeDays": int(active_days),
        },
        "calendar": calendar,
        "achievements": achievements,
        "tokens": tokens,
        "toolBreakdown": tool_breakdown,
        "folderLeaderboard": folder_leaderboard,
        "hourly": hourly,
        "dailyCost": daily_cost,
    }


def _ach(aid, name, desc, icon, actual, target):
    progress = min(1.0, actual / target) if target else 0.0
    return {
        "id": aid, "name": name, "desc": desc, "icon": icon,
        "unlocked": progress >= 1.0,
        "progress": round(progress, 3),
    }


# --------------------------------------------------------------------------- #
# Live sessions
# --------------------------------------------------------------------------- #

def _find_claude():
    """Locate the `claude` binary. Under launchd the PATH is minimal, so we
    check common install locations by absolute path before falling back to PATH."""
    import shutil
    cands = [
        os.path.expanduser("~/.claude/local/claude"),
        "/opt/homebrew/bin/claude",
        "/usr/local/bin/claude",
        os.path.expanduser("~/.local/bin/claude"),
        os.path.expanduser("~/.npm-global/bin/claude"),
    ]
    for p in cands:
        if os.path.exists(p):
            return p
    aug = os.environ.get("PATH", "") + \
        ":/opt/homebrew/bin:/usr/local/bin:" + os.path.expanduser("~/.local/bin")
    return shutil.which("claude", path=aug) or "claude"


def get_live_agents():
    """Return (agents_list, error_or_None)."""
    try:
        env = dict(os.environ)
        env["PATH"] = env.get("PATH", "") + \
            ":/opt/homebrew/bin:/usr/local/bin:" + os.path.expanduser("~/.local/bin")
        proc = subprocess.run(
            [_find_claude(), "agents", "--json"],
            capture_output=True, text=True, timeout=10, env=env,
        )
        if proc.returncode != 0:
            return [], f"claude agents exited {proc.returncode}: {proc.stderr.strip()[:200]}"
        data = json.loads(proc.stdout)
        if isinstance(data, list):
            return data, None
        return [], "unexpected output shape from claude agents --json"
    except FileNotFoundError:
        return [], "claude CLI not found on PATH"
    except subprocess.TimeoutExpired:
        return [], "claude agents --json timed out"
    except Exception as e:
        return [], f"claude agents error: {e}"


def build_session(agent, meals=None, fatigue_on=True):
    """Build one session object from a live agent + its transcript.
    `meals` is load_meals() (read once per build); `fatigue_on` mirrors
    config.creatureFatigue."""
    session_id = agent.get("sessionId") or agent.get("id") or ""
    short_id = (session_id or "")[:8] or "unknown"
    cwd = agent.get("cwd") or ""
    folder = os.path.basename(cwd.rstrip("/")) if cwd else ""
    kind = agent.get("kind") or "interactive"

    raw_status = ""
    if kind == "background":
        raw_status = agent.get("state") or ""
    else:
        raw_status = agent.get("status") or ""

    # status mapping
    if kind == "interactive":
        if raw_status == "busy":
            status = "working"
        elif raw_status in LIVE_WAIT_STATES:
            status = "needs"
        else:
            status = "idle"
    else:  # background
        if raw_status == "blocked":
            status = "needs"
        else:
            status = "idle"

    path = find_transcript(session_id)
    agg = scan_file(path)
    tx = parse_transcript(path)  # cached; cheap

    # last activity: prefer transcript; else startedAt
    last_activity_dt = tx["last_activity"]
    if last_activity_dt is None:
        started = agent.get("startedAt")
        if isinstance(started, (int, float)):
            last_activity_dt = datetime.fromtimestamp(started / 1000.0, tz=timezone.utc)

    if last_activity_dt is not None:
        age_secs = int((now_utc() - last_activity_dt).total_seconds())
        last_activity_iso = last_activity_dt.isoformat()
    else:
        age_secs = 0
        last_activity_iso = ""

    stale = age_secs > 86400
    if stale and status != "working":
        status = "stale"

    # --- alert detection: open question/plan, recent (age < 6h) error, else blocked note ---
    alert = None
    alert_kind = None
    waiting_since = None
    ask = agg.get("pending_ask") if isinstance(agg, dict) else None
    errors = agg.get("errors", []) if isinstance(agg, dict) else []
    cutoff = now_utc().timestamp() - 6 * 3600
    recent = None
    for ts, sig in errors:
        if ts >= cutoff and (recent is None or ts > recent[0]):
            recent = (ts, sig)
    if ask and status != "stale":
        # The tab is parked on a question or a plan approval until you answer it.
        status = "needs"
        alert_kind = ask.get("kind") or "question"
        if alert_kind == "plan":
            alert = "Waiting for you to approve a plan"
        else:
            alert = "Asked you a question" + (": " + ask["text"] if ask.get("text") else "")
        waiting_since = _iso_or_none(ask.get("since"))
    elif recent is not None:
        alert = 'Recent error detected: matched "%s" in session output' % recent[1]
        alert_kind = "error"
        status = "needs"  # force attention regardless of prior status
    elif status == "needs" and kind == "interactive":
        alert = "Waiting for your input"
        alert_kind = "waiting"
    elif status == "needs":
        alert = "Background agent is blocked / awaiting input"
        alert_kind = "blocked"

    # A permission-gated tool open for a while in a prompting permission mode MAY
    # be sitting on a permission prompt. A hint only: the status is unchanged.
    likely_awaiting = None
    gated = agg.get("gated_tool_open") if isinstance(agg, dict) else None
    if (gated and status == "working" and kind == "interactive"
            and now_utc().timestamp() - gated[0] >= LIKELY_AWAIT_SECS):
        likely_awaiting = "May be waiting for permission to run %s" % (gated[1] or "a tool")

    now_label = tx["now_label"] if status == "working" else None

    name = agent.get("name") or short_id
    title = tx["ai_title"] or "Untitled session"

    if isinstance(agg, dict) and agg:
        tokens = _session_tokens(agg)
        spark = _buckets_from_ts(agg.get("activity_ts", []), 12, 24 * 3600)
    else:
        tokens = {"output": 0, "input": 0, "cacheRead": 0, "total": 0,
                  "estCostUSD": 0.0, "model": ""}
        spark = [0] * 12

    creature = creature_for(session_id, tx["prompt_count"])
    if fatigue_on:
        # Only a busy interactive session with a tool still open keeps tiring
        # past its last record; a plain "busy" (e.g. a background workflow)
        # does not, so nothing collapses retroactively when it ends.
        ext = agg.get("open_tool_since") if (kind == "interactive" and raw_status == "busy"
                                             and isinstance(agg, dict)) else None
        creature["fatigue"] = _fatigue_safe(session_id, agg, ext,
                                            (meals or {}).get(session_id, ()))

    return {
        "id": short_id,
        "sessionId": session_id,
        "name": name,
        "title": title,
        "cwd": cwd,
        "folder": folder,
        "kind": kind,
        "pid": agent.get("pid") if isinstance(agent.get("pid"), int) else None,
        "status": status,
        "rawStatus": raw_status,
        "creature": creature,
        "firstPrompt": truncate(tx["first_prompt"] or "", LIVE_PROMPT_MAX),
        "lastPrompt": truncate(tx["last_prompt"] or (tx["first_prompt"] or ""), LIVE_PROMPT_MAX),
        "lastReply": truncate(tx["last_reply"] or "", LIVE_REPLY_MAX),
        "now": now_label,
        "promptCount": tx["prompt_count"],
        "lastActivity": last_activity_iso,
        "ageSecs": age_secs,
        "stale": stale,
        "links": tx["links"],
        "alert": alert,
        "alertKind": alert_kind,
        "waitingSince": waiting_since,
        "likelyAwaiting": likely_awaiting,
        "tokens": tokens,
        "spark": spark,
        "source": "claude",
        # session-meta (merged from sessions-meta.json in build_payload)
        "pinned": False,
        "tags": [],
        "note": "",
        # stuck detection (filled in build_payload using config.stuckMinutes)
        "stuck": False,
        "stuckReason": None,
    }


def build_feed(sessions, limit=25):
    """Build a merged recent-activity feed across the LIVE sessions (newest first)."""
    events = []
    for s in sessions:
        sid = s.get("sessionId")
        if not sid:
            continue
        try:
            path = find_transcript(sid)
            agg = scan_file(path)
        except Exception:
            agg = None
        if not isinstance(agg, dict):
            continue
        title = s.get("title") or agg.get("ai_title") or "Untitled session"
        for ev in agg.get("timeline", [])[-12:]:
            k = ev.get("kind")
            if k not in ("you", "claude", "tool"):
                continue
            events.append({
                "t": ev.get("t"),
                "sessionId": sid,
                "title": title,
                "kind": k,
                "text": ev.get("text") or "",
                "tool": ev.get("tool"),
                "source": s.get("source") or "claude",
            })
        if s.get("alert"):
            events.append({
                "t": s.get("lastActivity") or "",
                "sessionId": sid,
                "title": title,
                "kind": "needs",
                "text": s.get("alert"),
                "tool": None,
                "source": s.get("source") or "claude",
            })
    events.sort(key=lambda e: _epoch(e.get("t")), reverse=True)
    return events[:limit]


_ARCHIVED_CAP = 150  # most-recent archived transcripts to surface as stale cards

_cursor_title_cache = {"key": None, "names": {}}
_cursor_title_lock = threading.Lock()


def _sqlite_ro_uri(path):
    """file: URI for a read-only sqlite open. Spaces in the path are encoded."""
    return "file:%s?mode=ro" % path.replace(" ", "%20").replace("#", "%23").replace("?", "%3F")


def _read_cursor_composer_names(path):
    """composerId -> chat title from Cursor's composerHeaders table."""
    names = {}
    try:
        con = sqlite3.connect(_sqlite_ro_uri(path), uri=True, timeout=1.0)
    except Exception:
        return names
    try:
        try:
            rows = con.execute("SELECT composerId, value FROM composerHeaders")
        except sqlite3.Error:
            return names
        for cid, val in rows:
            if isinstance(val, (bytes, bytearray)):
                val = val.decode("utf-8", "replace")
            if not isinstance(cid, str) or not cid or not isinstance(val, str) or not val:
                continue
            try:
                obj = json.loads(val)
            except Exception:
                continue
            if not isinstance(obj, dict):
                continue
            name = obj.get("name")
            if not isinstance(name, str):
                continue
            name = " ".join(name.split())
            if name:
                names[cid] = name
    finally:
        try:
            con.close()
        except Exception:
            pass
    return names


def cursor_composer_names():
    """Cursor's own chat titles, cached until state.vscdb changes.

    Missing, locked, or older databases return {}. Callers then keep the
    first message as the heading. The lookup never writes and never leaves
    the machine."""
    path = CURSOR_STATE_DB
    try:
        st = os.stat(path)
    except OSError:
        return {}
    key = (path, getattr(st, "st_mtime_ns", st.st_mtime), st.st_size)
    with _cursor_title_lock:
        if _cursor_title_cache["key"] == key:
            return _cursor_title_cache["names"]
    names = _read_cursor_composer_names(path)
    with _cursor_title_lock:
        _cursor_title_cache["key"] = key
        _cursor_title_cache["names"] = names
    return names


def session_heading(agg, sid):
    """Heading for a card. Cursor uses the chat title it stored, when it has one."""
    fallback = "Untitled session"
    if isinstance(agg, dict):
        stored = agg.get("ai_title")
        if isinstance(stored, str) and stored.strip():
            fallback = stored
        if agg.get("source") == "cursor" and sid:
            named = cursor_composer_names().get(sid)
            if named:
                return named
    return fallback


def build_archived_session(path, sid, meals=None, fatigue_on=True, status=None):
    """Build a card for a transcript that is not a live `claude agents` session.

    `status` "working" or "idle" is a Cursor chat touched recently. Anything
    else stays a stale archive card."""
    agg = scan_file(path) or {}
    la = agg.get("last_activity")
    age = int((now_utc() - la).total_seconds()) if la else 0
    if isinstance(agg, dict) and agg:
        tokens = _session_tokens(agg)
        spark = _buckets_from_ts(agg.get("activity_ts", []), 12, 24 * 3600)
    else:
        tokens = {"output": 0, "input": 0, "cacheRead": 0, "total": 0,
                  "estCostUSD": 0.0, "model": ""}
        spark = [0] * 12
    creature = creature_for(sid, agg.get("prompt_count", 0))
    if fatigue_on:
        creature["fatigue"] = _fatigue_safe(sid, agg, None, (meals or {}).get(sid, ()))
    source = agg.get("source") or "claude"
    fresh = source == "cursor" and status in ("working", "idle")
    cwd = _path_from_project_slug(agg.get("folder") or "") if source == "cursor" else ""
    return {
        "id": (sid or "")[:8] or "unknown", "sessionId": sid,
        "name": "cursor" if source == "cursor" else ((sid or "")[:8] or "archived"),
        "title": session_heading(agg, sid),
        "cwd": cwd, "folder": agg.get("folder") or "",
        "kind": "cursor" if fresh else "archived", "pid": None,
        "status": status if fresh else "stale",
        "rawStatus": "cursor" if source == "cursor" else "archived",
        "creature": creature,
        "firstPrompt": truncate(agg.get("first_prompt") or "", LIVE_PROMPT_MAX),
        "lastPrompt": truncate(agg.get("last_prompt") or agg.get("first_prompt") or "",
                               LIVE_PROMPT_MAX),
        "lastReply": truncate(agg.get("last_reply") or "", LIVE_REPLY_MAX),
        "now": (agg.get("now_label") if status == "working" else None),
        "promptCount": agg.get("prompt_count", 0),
        "lastActivity": la.isoformat() if la else "", "ageSecs": age,
        "stale": not fresh, "links": (agg.get("links") or [])[:4],
        "alert": None, "alertKind": None, "tokens": tokens, "spark": spark,
        "archived": not fresh,
        "source": source,
    }


def build_payload():
    # Config first: creatureFatigue decides whether the meal ledger is read at all.
    try:
        config = load_config()
    except Exception:
        config = dict(DEFAULT_CONFIG)
    fz_on = bool(config.get("creatureFatigue", True))
    try:
        meals = load_meals() if fz_on else {}
    except Exception:
        meals = {}

    agents, error = get_live_agents()
    sessions = []
    for a in agents:
        try:
            sessions.append(build_session(a, meals=meals, fatigue_on=fz_on))
        except Exception:
            # never let one bad agent crash the whole payload
            continue

    # --- archived sessions: every past transcript (not currently live) as a stale card ---
    try:
        live_ids = set(s.get("sessionId") for s in sessions)
        arch = []
        for p in iter_transcript_paths():
            sid = _session_id_from_path(p)
            if not sid or sid in live_ids:
                continue
            agg = scan_file(p)
            if not agg or agg.get("prompt_count", 0) < 1:
                continue
            if agg.get("source") == "cursor":
                try:
                    touched = time.time() - os.path.getmtime(p)
                except Exception:
                    touched = CURSOR_IDLE_SECS + 1
                if touched <= CURSOR_IDLE_SECS:
                    st = "working" if touched <= CURSOR_WORKING_SECS else "idle"
                    try:
                        sessions.append(build_archived_session(
                            p, sid, meals=meals, fatigue_on=fz_on, status=st))
                    except Exception:
                        pass
                    live_ids.add(sid)
                    continue
            la = agg.get("last_activity")
            arch.append((la.timestamp() if la else 0.0, p, sid))
        arch.sort(key=lambda x: -x[0])
        for _, p, sid in arch[:_ARCHIVED_CAP]:
            try:
                sessions.append(build_archived_session(p, sid, meals=meals,
                                                       fatigue_on=fz_on))
            except Exception:
                continue
    except Exception:
        pass

    # sort: working first, then needs, then idle, then stale; newest activity first
    order = {"working": 0, "needs": 1, "idle": 2, "stale": 3}
    sessions.sort(key=lambda s: (order.get(s["status"], 9), -_epoch(s["lastActivity"])))

    try:
        season = compute_season()
    except Exception as e:
        season = _empty_season(str(e))

    try:
        feed = build_feed(sessions)
    except Exception:
        feed = []

    # --- session-meta merge + health (all additive; config loaded above) ---
    try:
        meta = load_meta()
    except Exception:
        meta = {}

    try:
        merge_evo_choices(sessions, load_evo_choices())
    except Exception:
        pass

    stuck_secs = int(config.get("stuckMinutes", 15)) * 60
    stuck_ids = []
    for s in sessions:
        sid = s.get("sessionId")
        m = meta.get(sid)
        if m:
            s["pinned"] = bool(m.get("pinned"))
            s["tags"] = list(m.get("tags") or [])
            s["note"] = m.get("note") or ""
            alias = (m.get("name") or "").strip()
            if alias:
                s["alias"] = alias
                s["autoTitle"] = s.get("title")  # keep the original for reference
                s["title"] = alias               # rename throughout HQ
        if s.get("status") == "working" and int(s.get("ageSecs", 0)) > stuck_secs:
            mins = int(s.get("ageSecs", 0)) // 60
            s["stuck"] = True
            s["stuckReason"] = "No activity for %d min while working" % mins
            if sid:
                stuck_ids.append(sid)

    # today's list-price cost estimate: reuse season's dailyCost (last = today)
    daily_cost = 0.0
    try:
        dc = season.get("dailyCost") or []
        if dc:
            daily_cost = float(dc[-1].get("usd", 0.0) or 0.0)
    except Exception:
        daily_cost = 0.0

    budget = float(config.get("dailyBudgetUSD", 0) or 0)
    over_budget = bool(budget > 0 and daily_cost > budget)
    budget_pct = round(daily_cost / budget, 4) if budget > 0 else 0.0
    warnings = []
    if stuck_ids:
        warnings.append("%d tab%s may be stuck"
                        % (len(stuck_ids), "" if len(stuck_ids) == 1 else "s"))
    if over_budget:
        warnings.append("over daily budget")

    health = {
        "stuck": stuck_ids,
        "stuckCount": len(stuck_ids),
        "dailyCostUSD": round(daily_cost, 4),
        "dailyBudgetUSD": budget,
        "overBudget": over_budget,
        "budgetPct": budget_pct,
        "warnings": warnings,
    }

    payload = {
        "updated": now_utc().isoformat(),
        "version": APP_VERSION,
        "boot": BOOT_ID,      # changes on every restart: open tabs reload on a new one
        "season": season,
        "sessions": sessions,
        "feed": feed,
        "health": health,
        "config": config,
    }
    if error:
        payload["error"] = error
    return payload


def _epoch(iso):
    dt = parse_ts(iso)
    return dt.timestamp() if dt else 0.0


# Short whole-payload memo so bursts of requests don't recompute everything.
# "gen" bumps on every invalidation, so a build that started before a meal was
# recorded can't be cached as fresh after it.
_payload_memo = {"ts": 0.0, "data": None, "gen": 0}
_payload_memo_lock = threading.Lock()


def _invalidate_payload_memo():
    with _payload_memo_lock:
        _payload_memo["ts"] = 0.0
        _payload_memo["gen"] += 1


def _stream_sig(payload):
    """What a stream client needs to see change: everything except the clock
    (`updated`) and the per-tick session ages, which drift every frame. Ages
    still reach the page on the heartbeat frame (STREAM_HEARTBEAT_SECS)."""
    slim = {k: v for k, v in payload.items() if k != "updated"}
    if isinstance(slim.get("sessions"), list):
        slim["sessions"] = [{k: v for k, v in s.items() if k != "ageSecs"}
                            if isinstance(s, dict) else s for s in slim["sessions"]]
    return json.dumps(slim, sort_keys=True)


_blob_memo = {"data": None, "blob": None, "sig": None}
_blob_memo_lock = threading.Lock()


def build_payload_blob():
    """(serialized payload, change signature), shared by every stream client so
    each tick serializes the payload once, not once per open tab."""
    data = build_payload_memo()
    with _blob_memo_lock:
        if _blob_memo["data"] is data:
            return _blob_memo["blob"], _blob_memo["sig"]
    blob, sig = json.dumps(data), _stream_sig(data)
    with _blob_memo_lock:
        _blob_memo.update(data=data, blob=blob, sig=sig)
    return blob, sig


def _full_text_sessions(sessions):
    """Copies of the payload sessions with untruncated prompt/reply text, for
    exports (the live payload trims them to keep each stream frame small)."""
    out = []
    for s in sessions or []:
        s = dict(s)
        sid = s.get("sessionId")
        path = find_transcript(sid) if sid else None
        agg = scan_file(path) if path else None
        if isinstance(agg, dict) and agg:
            s["firstPrompt"] = agg.get("first_prompt") or ""
            s["lastPrompt"] = agg.get("last_prompt") or agg.get("first_prompt") or ""
            s["lastReply"] = agg.get("last_reply") or ""
        out.append(s)
    return out


def build_payload_memo():
    now = time.monotonic()
    with _payload_memo_lock:
        if _payload_memo["data"] is not None and (now - _payload_memo["ts"]) < 1.5:
            return _payload_memo["data"]
        gen0 = _payload_memo["gen"]
    data = build_payload()
    with _payload_memo_lock:
        _payload_memo["ts"] = time.monotonic() if _payload_memo["gen"] == gen0 else 0.0
        _payload_memo["data"] = data
    return data


def build_session_detail(sid):
    """Build the single-session detail payload, or None if unknown."""
    path = find_transcript(sid)
    if not path:
        return None
    agg = scan_file(path)
    if agg is None:
        return None

    # Reuse live-derived fields (status/kind/cwd/links/title) when the session is live.
    try:
        payload = build_payload_memo()
        sess = next((s for s in payload.get("sessions", [])
                     if s.get("sessionId") == sid), None)
    except Exception:
        sess = None

    if sess:
        folder = sess.get("folder", "") or agg["folder"]
        cwd = sess.get("cwd", "")
        kind = sess.get("kind", "interactive")
        status = sess.get("status", "idle")
        links = sess.get("links", []) or agg["links"][:4]
        title = sess.get("title") or session_heading(agg, sid)
        creature = sess.get("creature") or creature_for(sid, agg["prompt_count"])
    else:
        folder = agg["folder"]
        cwd = ""
        kind = "interactive"
        links = agg["links"][:4]
        title = session_heading(agg, sid)
        la = agg["last_activity"]
        age = int((now_utc() - la).total_seconds()) if la else 0
        status = "stale" if age > 86400 else "idle"
        cutoff = now_utc().timestamp() - 6 * 3600
        if any(ts >= cutoff for ts, _ in agg["errors"]):
            status = "needs"
        creature = creature_for(sid, agg["prompt_count"])
        fz = _offpayload_fatigue(sid, agg)
        if fz is not None:
            creature["fatigue"] = fz

    files = sorted(agg["files"].values(), key=lambda x: -x["count"])[:15]

    # timing/history metadata
    ts_list = agg.get("activity_ts", []) or []
    first_iso = ""
    span_days = 0
    if ts_list:
        first_ts = min(ts_list)
        last_ts = max(ts_list)
        first_iso = datetime.fromtimestamp(first_ts, tz=timezone.utc).isoformat()
        span_days = int((last_ts - first_ts) // 86400)
    active_days = len(agg.get("per_day", {}) or {})

    return {
        "id": (sid or "")[:8] or "unknown",
        "sessionId": sid,
        "title": title,
        "folder": folder,
        "cwd": cwd,
        "kind": kind,
        "status": status,
        "model": agg["model"] or "",
        "tokens": _session_tokens(agg),
        "timeline": agg["timeline"][-20:],
        "files": files,
        "links": links,
        "sparkHourly": _buckets_from_ts(agg.get("activity_ts", []), 24, 24 * 3600),
        "resumeCmd": ("" if (agg.get("source") == "cursor")
                      else "claude --resume %s" % sid),
        "lastReplyFull": agg["last_reply"] or "",
        "firstPromptFull": agg.get("first_prompt") or "",
        "lastPromptFull": agg.get("last_prompt") or agg.get("first_prompt") or "",
        "firstActivity": first_iso,
        "lastActivity": (agg["last_activity"].isoformat() if agg.get("last_activity") else ""),
        "spanDays": span_days,
        "activeDays": active_days,
        "promptCount": agg.get("prompt_count", 0),
        "creature": creature,
        "source": (sess.get("source") if isinstance(sess, dict) and sess.get("source")
                   else agg.get("source") or "claude"),
    }


def _empty_season(err=None):
    today = now_utc().date()
    calendar = []
    for i in range(13, -1, -1):
        d = today - timedelta(days=i)
        calendar.append({"date": d.isoformat(), "count": 0, "heat": 0, "today": d == today})
    daily_cost = []
    for i in range(13, -1, -1):
        d = today - timedelta(days=i)
        daily_cost.append({"date": d.isoformat(), "usd": 0.0})
    s = {
        "level": 1, "xp": 0, "xpIntoLevel": 0, "xpForLevel": xp_for_level(1),
        "pct": 0.0, "rank": rank_for_level(1), "streak": 0, "bestStreak": 0,
        "totals": {"prompts": 0, "tools": 0, "artifacts": 0, "activeDays": 0},
        "calendar": calendar, "achievements": [],
        "tokens": {"output": 0, "input": 0, "cacheRead": 0, "total": 0, "estCostUSD": 0.0},
        "toolBreakdown": [], "folderLeaderboard": [], "hourly": [0] * 24,
        "dailyCost": daily_cost,
    }
    if err:
        s["error"] = err
    return s


# --------------------------------------------------------------------------- #
# Full-history search index (cached by file mtime/size), over ALL transcripts.
# --------------------------------------------------------------------------- #

_search_cache = {}
_search_lock = threading.Lock()
_SEARCH_MAX_BLOB = 200_000  # cap stored text per file to keep memory bounded


def _session_id_from_path(path):
    base = os.path.basename(path)
    return base[:-6] if base.endswith(".jsonl") else base


def _build_search_entry(path):
    """Read one transcript, collecting ai-title + human prompts + assistant text."""
    if _is_cursor_transcript(path):
        return _build_cursor_search_entry(path)
    title = None
    last_activity = None
    parts = []
    total = 0
    try:
        f = open(path, "r", encoding="utf-8", errors="replace")
    except Exception:
        return None
    with f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            if not isinstance(o, dict):
                continue
            try:
                typ = o.get("type")
                ts = parse_ts(o.get("timestamp"))
                if ts and (last_activity is None or ts > last_activity):
                    last_activity = ts
                if total >= _SEARCH_MAX_BLOB:
                    continue
                if typ == "ai-title":
                    at = o.get("aiTitle")
                    if at:
                        title = at
                        parts.append(at)
                        total += len(at)
                elif typ == "user":
                    content = (o.get("message") or {}).get("content")
                    if is_real_human_prompt(content):
                        c = clean_prompt(content)
                        parts.append(c)
                        total += len(c)
                elif typ == "assistant":
                    blocks = (o.get("message") or {}).get("content")
                    if isinstance(blocks, list):
                        for b in blocks:
                            if isinstance(b, dict) and b.get("type") == "text":
                                t = b.get("text") or ""
                                if t:
                                    st = strip_markdown(t)
                                    parts.append(st)
                                    total += len(st)
            except Exception:
                continue
    text = " \n ".join(parts)[:_SEARCH_MAX_BLOB]
    return {
        "sessionId": _session_id_from_path(path),
        "title": title or "Untitled session",
        "folder": os.path.basename(os.path.dirname(path)),
        "lastActivity": last_activity.isoformat() if last_activity else "",
        "text": text,
        "blob": text.lower(),
        "source": "claude",
    }


def _build_cursor_search_entry(path):
    """Search blob for one Cursor agent transcript. Same shape as Claude's."""
    title = None
    last_activity = None
    parts = []
    total = 0
    try:
        f = open(path, "r", encoding="utf-8", errors="replace")
    except Exception:
        return None
    with f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            if not isinstance(o, dict):
                continue
            try:
                role = o.get("role")
                if role == "user":
                    text = _cursor_message_text(o)
                    ts = _parse_cursor_clock(text)
                    if ts and (last_activity is None or ts > last_activity):
                        last_activity = ts
                    query = _cursor_user_query(text)
                    if query and total < _SEARCH_MAX_BLOB:
                        if title is None:
                            title = truncate(query, 80)
                        parts.append(query)
                        total += len(query)
                elif role == "assistant" and total < _SEARCH_MAX_BLOB:
                    blocks = (o.get("message") or {}).get("content")
                    if isinstance(blocks, str):
                        blocks = [{"type": "text", "text": blocks}]
                    if isinstance(blocks, list):
                        for b in blocks:
                            if isinstance(b, dict) and b.get("type") == "text":
                                t = b.get("text") or ""
                                if t:
                                    st = strip_markdown(t)
                                    parts.append(st)
                                    total += len(st)
            except Exception:
                continue
    if last_activity is None:
        try:
            last_activity = datetime.fromtimestamp(os.path.getmtime(path), tz=timezone.utc)
        except Exception:
            last_activity = None
    text = " \n ".join(parts)[:_SEARCH_MAX_BLOB]
    return {
        "sessionId": _session_id_from_path(path),
        "title": title or "Untitled session",
        "folder": _cursor_project_slug(path),
        "lastActivity": last_activity.isoformat() if last_activity else "",
        "text": text,
        "blob": text.lower(),
        "source": "cursor",
    }


def get_search_entry(path):
    """Cached search entry for one file, rebuilt only when (mtime,size) changed."""
    try:
        st = os.stat(path)
    except Exception:
        return None
    key = (st.st_mtime, st.st_size)
    with _search_lock:
        cached = _search_cache.get(path)
        if cached is not None and cached.get("_key") == key:
            return cached
    entry = _build_search_entry(path)
    if entry is not None:
        entry["_key"] = key
        with _search_lock:
            _search_cache[path] = entry
    return entry


def _live_status_map():
    """Map sessionId -> live status, from the memoized live payload."""
    out = {}
    try:
        payload = build_payload_memo()
        for s in payload.get("sessions", []):
            sid = s.get("sessionId")
            if sid:
                out[sid] = s.get("status", "idle")
    except Exception:
        pass
    return out


_TERM_RE = re.compile(r"[a-z0-9_]+")


def _snippet_around(text, idx, term_len):
    start = max(0, idx - 70)
    end = min(len(text), idx + term_len + 90)
    snip = re.sub(r"\s+", " ", text[start:end]).strip()
    if start > 0:
        snip = "…" + snip
    if end < len(text):
        snip = snip + "…"
    return snip


def search_transcripts(q, limit=40):
    """TF-IDF ranked search across ALL transcripts. Query is split into terms;
    documents are scored by sum(tf * idf) so rarer terms weigh more and docs
    matching more of the query rank higher. Response shape is unchanged."""
    ql = (q or "").lower()
    terms = _TERM_RE.findall(ql)
    if not terms:
        # fall back to a raw substring if the query has no word chars
        terms = [ql.strip()] if ql.strip() else []
    terms = list(dict.fromkeys(terms))  # dedupe, keep order
    if not terms:
        return []

    live_map = _live_status_map()

    # First pass: gather candidate entries + per-term document frequencies.
    entries = []
    df = {t: 0 for t in terms}
    for path in iter_transcript_paths():
        entry = get_search_entry(path)
        if entry is None:
            continue
        blob = entry["blob"]
        tf = {}
        for t in terms:
            c = blob.count(t)
            if c:
                tf[t] = c
                df[t] += 1
        if tf:
            entries.append((entry, tf))

    n_docs = max(1, len(entries))
    import math
    idf = {t: math.log(1.0 + n_docs / (1.0 + df[t])) for t in terms}

    results = []
    for entry, tf in entries:
        score = 0.0
        matches = 0
        for t, c in tf.items():
            score += (1.0 + math.log(c)) * idf[t]
            matches += c
        # bonus for covering more distinct query terms
        score *= (1.0 + 0.5 * (len(tf) - 1))

        blob = entry["blob"]
        text = entry["text"]
        # snippet around the first occurrence of the rarest matched term
        best_t = min(tf.keys(), key=lambda t: (df[t], -len(t)))
        idx = blob.find(best_t)
        if idx < 0:
            idx = 0
        sid = entry["sessionId"]
        title = entry["title"]
        if entry.get("source") == "cursor":
            named = cursor_composer_names().get(sid)
            if named:
                title = named
        results.append({
            "sessionId": sid,
            "title": title,
            "folder": entry["folder"],
            "lastActivity": entry["lastActivity"],
            "live": sid in live_map,
            "status": live_map.get(sid, "archived"),
            "snippet": _snippet_around(text, idx, len(best_t)),
            "matches": int(matches),
            "source": entry.get("source") or "claude",
            "_score": score,
        })
    results.sort(key=lambda r: (-r["_score"], -_epoch(r["lastActivity"])))
    for r in results:
        r.pop("_score", None)
    return results[:limit]


# --------------------------------------------------------------------------- #
# History / analytics (91-day heatmap, 30-day daily, hall of fame all-time)
# --------------------------------------------------------------------------- #

def _model_family(model):
    """Normalize a model id to a short family label for breakdowns."""
    m = (model or "").lower()
    if not m:
        return "unknown"
    if "opus" in m:
        return "Opus"
    if "sonnet" in m:
        return "Sonnet"
    if "haiku" in m:
        return "Haiku"
    if "fable" in m:
        return "Fable"
    return model


def compute_history():
    today = now_utc().date()
    start91 = today - timedelta(days=90)   # 91-day inclusive window
    start30 = today - timedelta(days=29)   # 30-day inclusive window

    day_heat = {}     # date -> prompts + replies
    day_daily = {}    # date -> {output,cost,prompts,tools} (last 30d only)
    byhour = [0] * 24
    bydow = [0] * 7   # Monday=0 .. Sunday=6
    tot_output = tot_prompts = tot_tools = 0
    tot_cost = 0.0
    active = set()
    hall = []
    # all-time breakdowns
    model_out, model_cost, model_sess = {}, {}, {}
    folder_cost, folder_out = {}, {}

    files = list(iter_transcript_paths())
    for path in files:
        agg = scan_file(path)
        if agg is None:
            continue
        tools_all = 0
        for dd in agg.get("per_day", {}).values():
            tools_all += dd.get("tools", 0)
        # all-time model + folder cost attribution (per file's totals)
        fam = _model_family(agg.get("model") or "")
        fout = int(agg.get("tok_output", 0))
        fcost = agg.get("cost", 0.0)
        model_out[fam] = model_out.get(fam, 0) + fout
        model_cost[fam] = model_cost.get(fam, 0.0) + fcost
        model_sess[fam] = model_sess.get(fam, 0) + 1
        fol = agg.get("folder") or ""
        folder_cost[fol] = folder_cost.get(fol, 0.0) + fcost
        folder_out[fol] = folder_out.get(fol, 0) + fout
        hall.append({
            "sessionId": _session_id_from_path(path),
            "title": session_heading(agg, _session_id_from_path(path)),
            "folder": agg.get("folder") or "",
            "output": int(agg.get("tok_output", 0)),
            "estCostUSD": round(agg.get("cost", 0.0), 4),
            "tools": int(tools_all),
        })
        for diso, dd in agg.get("per_day", {}).items():
            try:
                d = date.fromisoformat(diso)
            except Exception:
                continue
            if d < start91 or d > today:
                continue
            p = dd.get("prompts", 0)
            r = dd.get("replies", 0)
            t = dd.get("tools", 0)
            o = dd.get("output", 0)
            c = dd.get("cost", 0.0)
            day_heat[d] = day_heat.get(d, 0) + p + r
            tot_output += o
            tot_prompts += p
            tot_tools += t
            tot_cost += c
            if p or r or t:
                active.add(d)
            dow = d.weekday()
            for h, hc in dd.get("hours", {}).items():
                if 0 <= h < 24:
                    byhour[h] += hc
                    bydow[dow] += hc
            if d >= start30:
                e = day_daily.setdefault(
                    d, {"output": 0, "cost": 0.0, "prompts": 0, "tools": 0})
                e["output"] += o
                e["cost"] += c
                e["prompts"] += p
                e["tools"] += t

    heat_counts = [day_heat.get(start91 + timedelta(days=i), 0) for i in range(91)]
    mx = max(heat_counts) if heat_counts else 0
    heatmap = []
    for i in range(91):
        d = start91 + timedelta(days=i)
        c = heat_counts[i]
        if c <= 0 or mx <= 0:
            level = 0
        else:
            frac = c / mx
            level = 4 if frac >= 0.75 else 3 if frac >= 0.5 else 2 if frac >= 0.25 else 1
        heatmap.append({"date": d.isoformat(), "count": int(c), "level": level})

    daily = []
    for i in range(30):
        d = start30 + timedelta(days=i)
        e = day_daily.get(d)
        if e:
            daily.append({
                "date": d.isoformat(), "output": int(e["output"]),
                "cost": round(e["cost"], 4), "prompts": int(e["prompts"]),
                "tools": int(e["tools"]),
            })
        else:
            daily.append({
                "date": d.isoformat(), "output": 0, "cost": 0.0,
                "prompts": 0, "tools": 0,
            })

    hall.sort(key=lambda x: -x["output"])

    model_breakdown = sorted(
        ({"model": m, "output": int(model_out[m]),
          "estCostUSD": round(model_cost[m], 2), "sessions": int(model_sess[m])}
         for m in model_out),
        key=lambda x: -x["estCostUSD"])
    cost_by_folder = sorted(
        ({"folder": f, "estCostUSD": round(folder_cost[f], 2),
          "output": int(folder_out[f])}
         for f in folder_cost),
        key=lambda x: -x["estCostUSD"])[:8]

    return {
        "heatmap": heatmap,
        "daily": daily,
        "byHour": byhour,
        "byDow": bydow,
        "totals": {
            "transcripts": len(files),
            "activeDays": len(active),
            "output": int(tot_output),
            "estCostUSD": round(tot_cost, 2),
            "prompts": int(tot_prompts),
            "tools": int(tot_tools),
        },
        "hallOfFame": hall[:10],
        "modelBreakdown": model_breakdown,
        "costByFolder": cost_by_folder,
    }


def compute_pokedex():
    """Collection view across ALL transcripts. Each transcript maps to a species
    by the SAME hash(sessionId) % 48 used by creature_for(). Backed by scan_file's
    (mtime,size) cache so it stays responsive."""
    species = []
    for i in range(48):
        species.append({
            "species": i,
            "name": SPECIES_NAMES[i],
            "type": SPECIES_TYPES[i],
            "seed": int(species_seed(i)),
            "typeHue": int(TYPE_HUES.get(SPECIES_TYPES[i], 45)),
            "caught": False,
            "count": 0,
            "maxStage": 0,
            "totalOutput": 0,
            "shiny": False,
            "exampleSessionId": None,
            "_bestOut": -1,
        })

    for path in iter_transcript_paths():
        sid = _session_id_from_path(path)
        agg = scan_file(path)
        if agg is None:
            continue
        h = _session_hash(sid)
        sp = h % 48
        shiny = shiny_for_species(sp)   # per-species (matches creature_for)
        stage, _, _ = stage_for(agg.get("prompt_count", 0))
        out = int(agg.get("tok_output", 0))
        d = species[sp]
        d["count"] += 1
        d["totalOutput"] += out
        if stage > d["maxStage"]:
            d["maxStage"] = stage
        if shiny:
            d["shiny"] = True
        # keep the highest-output session as the representative example
        if out > d["_bestOut"] or d["exampleSessionId"] is None:
            d["_bestOut"] = out
            d["exampleSessionId"] = sid

    caught_count = 0
    shiny_count = 0
    for d in species:
        d["caught"] = d["count"] > 0
        if d["caught"]:
            caught_count += 1
        if d["shiny"]:
            shiny_count += 1
        d.pop("_bestOut", None)

    return {
        "caughtCount": int(caught_count),
        "total": 48,
        "shinyCount": int(shiny_count),
        "species": species,
    }


def _known_folders():
    """The real set of project-dir basenames that actually contain transcripts.
    Used to validate a ?folder= slug (also prevents traversal: we only ever
    match against known dirs, never build a path from the raw slug)."""
    out = set()
    for p in iter_transcript_paths():
        if _is_cursor_transcript(p):
            slug = _cursor_project_slug(p)
        else:
            slug = os.path.basename(os.path.dirname(p))
        if slug:
            out.add(slug)
    return out


def compute_project(slug):
    """Per-project rollup across every transcript whose folder == slug.
    Returns None if the slug is not a known folder (caller -> 404).
    Reuses scan_file's (mtime,size) cache; never raises for bad data."""
    if not slug or slug not in _known_folders():
        return None

    today = now_utc().date()
    start91 = today - timedelta(days=90)   # 91-day inclusive window

    day_heat = {}          # date -> prompts + replies (THIS folder)
    active = set()         # distinct active dates (all-time)
    files_agg = {}         # path -> {path, action, count}
    model_out, model_cost, model_sess = {}, {}, {}
    sessions = []
    tot_prompts = tot_tools = tot_output = 0
    tot_cost = 0.0
    n_sessions = 0

    for path in iter_transcript_paths():
        folder = (_cursor_project_slug(path) if _is_cursor_transcript(path)
                  else os.path.basename(os.path.dirname(path)))
        if folder != slug:
            continue
        agg = scan_file(path)
        if agg is None:
            continue
        n_sessions += 1

        # per-session tool total (sum across days)
        sess_tools = 0
        for dd in agg.get("per_day", {}).values():
            sess_tools += int(dd.get("tools", 0))

        # all-time totals + heatmap window
        for diso, dd in agg.get("per_day", {}).items():
            p = int(dd.get("prompts", 0))
            r = int(dd.get("replies", 0))
            t = int(dd.get("tools", 0))
            o = int(dd.get("output", 0))
            c = float(dd.get("cost", 0.0))
            tot_prompts += p
            tot_tools += t
            tot_output += o
            tot_cost += c
            try:
                d = date.fromisoformat(diso)
            except Exception:
                continue
            if p or r or t:
                active.add(d)
            if start91 <= d <= today:
                day_heat[d] = day_heat.get(d, 0) + p + r

        # files touched across this session
        for fe in (agg.get("files") or {}).values():
            key = fe.get("path")
            if not key:
                continue
            cur = files_agg.get(key)
            if cur is None:
                files_agg[key] = {
                    "path": key,
                    "action": fe.get("action") or "other",
                    "count": int(fe.get("count", 0)),
                }
            else:
                cur["count"] += int(fe.get("count", 0))

        # model attribution (per-file family)
        fam = _model_family(agg.get("model") or "")
        model_out[fam] = model_out.get(fam, 0) + int(agg.get("tok_output", 0))
        model_cost[fam] = model_cost.get(fam, 0.0) + float(agg.get("cost", 0.0))
        model_sess[fam] = model_sess.get(fam, 0) + 1

        la = agg.get("last_activity")
        sessions.append({
            "sessionId": _session_id_from_path(path),
            "title": session_heading(agg, _session_id_from_path(path)),
            "output": int(agg.get("tok_output", 0)),
            "estCostUSD": round(float(agg.get("cost", 0.0)), 4),
            "tools": int(sess_tools),
            "lastActivity": la.isoformat() if la else "",
        })

    # 91-day heatmap (oldest -> newest), levelled against the folder's own max
    heat_counts = [day_heat.get(start91 + timedelta(days=i), 0) for i in range(91)]
    mx = max(heat_counts) if heat_counts else 0
    heatmap = []
    for i in range(91):
        d = start91 + timedelta(days=i)
        c = heat_counts[i]
        if c <= 0 or mx <= 0:
            level = 0
        else:
            frac = c / mx
            level = 4 if frac >= 0.75 else 3 if frac >= 0.5 else 2 if frac >= 0.25 else 1
        heatmap.append({"date": d.isoformat(), "count": int(c), "level": level})

    top_files = sorted(files_agg.values(), key=lambda x: -x["count"])[:15]

    models = sorted(
        ({"model": m, "output": int(model_out[m]),
          "estCostUSD": round(model_cost[m], 4), "sessions": int(model_sess[m])}
         for m in model_out),
        key=lambda x: -x["estCostUSD"])

    sessions.sort(key=lambda s: -_epoch(s.get("lastActivity")))
    sessions = sessions[:40]

    return {
        "folder": slug,
        "prettyFolder": _pretty_folder(slug),
        "totals": {
            "sessions": int(n_sessions),
            "prompts": int(tot_prompts),
            "tools": int(tot_tools),
            "output": int(tot_output),
            "estCostUSD": round(tot_cost, 4),
            "activeDays": int(len(active)),
        },
        "heatmap": heatmap,
        "topFiles": top_files,
        "models": models,
        "sessions": sessions,
    }


def build_export_csv(payload):
    """CSV: one header + one row per LIVE session."""
    import csv
    import io
    buf = io.StringIO()
    w = csv.writer(buf)
    w.writerow([
        "id", "title", "folder", "status", "promptCount", "ageSecs",
        "outputTokens", "estCostUSD", "model", "lastActivity",
    ])
    for s in payload.get("sessions", []):
        tok = s.get("tokens", {}) or {}
        w.writerow([
            s.get("sessionId", ""),
            s.get("title", ""),
            s.get("folder", ""),
            s.get("status", ""),
            s.get("promptCount", 0),
            s.get("ageSecs", 0),
            tok.get("output", 0),
            tok.get("estCostUSD", 0.0),
            tok.get("model", "") or "",
            s.get("lastActivity", ""),
        ])
    return buf.getvalue()


# --------------------------------------------------------------------------- #
# Daily digest — "what I did across all Claude sessions on <date>"
# --------------------------------------------------------------------------- #

def _pretty_folder(slug):
    """Turn a project-dir slug into a readable label (lossy, best-effort)."""
    if not slug:
        return "~"
    m = re.sub(r"^-?Users-[^-]+-?", "", str(slug))
    return m if m else "~ (home)"


def _digest_day_detail(path, diso):
    """Back-compat single-day wrapper around _digest_range_detail."""
    return _digest_range_detail(path, {diso})


def _digest_range_detail(path, date_set):
    """Re-read one transcript, extracting the first human prompt, last assistant
    reply, and files touched across the given set of local-day iso strings. Cheap:
    only called for the handful of sessions active in the target range. When
    date_set has a single date this is identical to the old per-day behaviour."""
    first_prompt = None
    last_reply = None
    files = []
    seen_files = set()
    try:
        f = open(path, "r", encoding="utf-8", errors="replace")
    except Exception:
        return first_prompt, last_reply, files
    with f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            if not isinstance(o, dict):
                continue
            try:
                ts = parse_ts(o.get("timestamp"))
                if ts is None or ts.date().isoformat() not in date_set:
                    continue
                typ = o.get("type")
                if typ == "user":
                    content = (o.get("message") or {}).get("content")
                    if is_real_human_prompt(content) and first_prompt is None:
                        first_prompt = clean_prompt(content)
                elif typ == "assistant":
                    blocks = (o.get("message") or {}).get("content")
                    if isinstance(blocks, list):
                        texts = []
                        for b in blocks:
                            if not isinstance(b, dict):
                                continue
                            if b.get("type") == "text":
                                texts.append(b.get("text") or "")
                            elif b.get("type") == "tool_use":
                                inp = b.get("input") or {}
                                p = (inp.get("file_path") or inp.get("path")
                                     if isinstance(inp, dict) else None)
                                if isinstance(p, str) and p.strip():
                                    bn = os.path.basename(p.rstrip("/"))
                                    if bn and bn not in seen_files:
                                        seen_files.add(bn)
                                        files.append(bn)
                        if texts:
                            last_reply = strip_markdown("\n".join(texts))
            except Exception:
                continue
    return first_prompt, last_reply, files


def compute_digest(diso, days=1):
    """Build the digest payload (markdown + structured). With days=1 this is the
    single-day daily digest (unchanged). With days>1 it aggregates the last <days>
    days ENDING at <diso>, summing per-session stats across the range and including
    only sessions active somewhere in the range."""
    try:
        days = int(days)
    except Exception:
        days = 1
    days = max(1, min(31, days))

    try:
        end_d = date.fromisoformat(diso)
    except Exception:
        end_d = now_utc().astimezone().date()
        diso = end_d.isoformat()
    start_d = end_d - timedelta(days=days - 1)
    date_set = {(start_d + timedelta(days=i)).isoformat() for i in range(days)}

    entries = []
    tot = {"prompts": 0, "tools": 0, "output": 0, "cost": 0.0}
    for path in iter_transcript_paths():
        agg = scan_file(path)
        if agg is None:
            continue
        per_day = agg.get("per_day") or {}
        s_prompts = s_tools = s_output = 0
        s_cost = 0.0
        present = False
        for di in date_set:
            day = per_day.get(di)
            if not day:
                continue
            present = True
            s_prompts += day.get("prompts", 0)
            s_tools += day.get("tools", 0)
            s_output += day.get("output", 0)
            s_cost += day.get("cost", 0.0)
        if not present:
            continue
        fp, lr, files = _digest_range_detail(path, date_set)
        entries.append({
            "sessionId": _session_id_from_path(path),
            "title": session_heading(agg, _session_id_from_path(path)),
            "folder": _pretty_folder(agg.get("folder")),
            "prompts": s_prompts,
            "tools": s_tools,
            "output": s_output,
            "cost": round(s_cost, 4),
            "firstPrompt": fp or agg.get("first_prompt") or "",
            "lastReply": lr or agg.get("last_reply") or "",
            "files": files[:8],
            "links": [l.get("url") for l in (agg.get("links") or [])][:4],
        })
        tot["prompts"] += s_prompts
        tot["tools"] += s_tools
        tot["output"] += s_output
        tot["cost"] += s_cost

    entries.sort(key=lambda e: (-(e["prompts"] + e["tools"]), -e["output"]))

    if days == 1:
        lines = ["# Claude HQ — Daily Digest — %s" % diso, ""]
        empty_span = diso
    else:
        lines = ["# Claude HQ — Digest — %s → %s"
                 % (start_d.isoformat(), diso), ""]
        empty_span = "%s → %s" % (start_d.isoformat(), diso)
    if not entries:
        lines.append("_No session activity on %s._" % empty_span)
    else:
        lines.append(
            "**%d session%s active** · %d prompts · %d tool calls · ~%s output tokens · ~$%.2f list-price est."
            % (len(entries), "" if len(entries) == 1 else "s",
               tot["prompts"], tot["tools"], f"{tot['output']:,}", tot["cost"]))
        lines.append("")
        for e in entries:
            lines.append("## %s  (%s)" % (e["title"], e["folder"]))
            if e["firstPrompt"]:
                lines.append("- Started with: %s" % truncate(e["firstPrompt"], 220))
            if e["lastReply"]:
                lines.append("- Last outcome: %s" % truncate(e["lastReply"], 220))
            lines.append("- %d prompts · %d tools · ~%s output tok · ~$%.2f"
                         % (e["prompts"], e["tools"], f"{e['output']:,}", e["cost"]))
            if e["files"]:
                lines.append("- Files touched: %s" % ", ".join(e["files"]))
            if e["links"]:
                lines.append("- Links: %s" % " ".join(e["links"]))
            lines.append("")
    return {
        "date": diso,
        "days": days,
        "markdown": "\n".join(lines),
        "sessionCount": len(entries),
        "totals": {
            "prompts": tot["prompts"], "tools": tot["tools"],
            "output": tot["output"], "estCostUSD": round(tot["cost"], 2),
        },
    }


def compute_insights():
    """A handful of genuinely useful observations computed from the scan_file
    cache over ALL transcripts + the live payload. Each insight is guarded so it
    only appears when it has data. Never raises (falls back to empty list)."""
    try:
        today = now_utc().date()
        d7_start = today - timedelta(days=6)     # this week: [today-6 .. today]
        d14_start = today - timedelta(days=13)   # last week: [today-13 .. today-7]
        last_week_end = today - timedelta(days=7)
        d30_start = today - timedelta(days=29)

        day_cost = {}       # date -> est USD
        day_activity = {}   # date -> prompts + replies
        day_output = {}     # date -> output tokens
        folder_last = {}    # folder -> most recent activity date
        folder_week = {}    # folder -> prompts + tools in last 7d
        hourly = [0] * 24   # local-hour activity over 30d

        for path in iter_transcript_paths():
            agg = scan_file(path)
            if agg is None:
                continue
            fol = agg.get("folder") or ""
            la = agg.get("last_activity")
            if la is not None:
                ld = la.date()
                if fol not in folder_last or ld > folder_last[fol]:
                    folder_last[fol] = ld
            for diso, dd in (agg.get("per_day") or {}).items():
                try:
                    d = date.fromisoformat(diso)
                except Exception:
                    continue
                if d > today:
                    continue
                p = dd.get("prompts", 0)
                t = dd.get("tools", 0)
                r = dd.get("replies", 0)
                day_cost[d] = day_cost.get(d, 0.0) + dd.get("cost", 0.0)
                day_activity[d] = day_activity.get(d, 0) + p + r
                day_output[d] = day_output.get(d, 0) + dd.get("output", 0)
                if d7_start <= d <= today:
                    folder_week[fol] = folder_week.get(fol, 0) + p + t
                if d30_start <= d <= today:
                    for h, hc in (dd.get("hours") or {}).items():
                        if 0 <= h < 24:
                            hourly[h] += hc

        insights = []

        # --- spend: this week vs last week ---
        this_week = sum(v for d, v in day_cost.items() if d7_start <= d <= today)
        last_week = sum(v for d, v in day_cost.items()
                        if d14_start <= d <= last_week_end)
        if this_week > 0 or last_week > 0:
            if last_week > 0:
                pct = (this_week - last_week) / last_week * 100.0
                arrow = "▲" if pct >= 0 else "▼"
                detail = ("Spend $%.2f this week (%s %d%% vs last week's $%.2f)"
                          % (this_week, arrow, abs(int(round(pct))), last_week))
                kind = "warn" if pct > 25 else "info"
            else:
                detail = "Spend $%.2f this week (nothing last week)" % this_week
                kind = "info"
            insights.append({"icon": "💸", "title": "Weekly spend",
                             "detail": detail, "kind": kind})

        # --- busiest project this week ---
        if folder_week:
            top_fol = max(folder_week, key=lambda k: folder_week[k])
            score = folder_week[top_fol]
            if score > 0:
                insights.append({
                    "icon": "🔥", "title": "Busiest project",
                    "detail": "%s — %d prompts + tool calls in the last 7 days"
                    % (_pretty_folder(top_fol), score),
                    "kind": "good"})

        # --- dormant projects (untouched > 14 days), up to 2 ---
        dormant = []
        for fol, ld in folder_last.items():
            age = (today - ld).days
            if age > 14:
                dormant.append((age, fol))
        dormant.sort(reverse=True)
        for age, fol in dormant[:2]:
            insights.append({
                "icon": "💤", "title": "Dormant project",
                "detail": "%s untouched %d days" % (_pretty_folder(fol), age),
                "kind": "warn"})

        # --- biggest day in the last 30 ---
        biggest = None
        for d, v in day_activity.items():
            if d30_start <= d <= today and v > 0:
                if biggest is None or v > biggest[1]:
                    biggest = (d, v)
        if biggest:
            insights.append({
                "icon": "📈", "title": "Biggest day",
                "detail": "%s was your busiest — %d prompts + replies"
                % (biggest[0].isoformat(), biggest[1]),
                "kind": "info"})

        # --- output this week ---
        out_week = sum(v for d, v in day_output.items() if d7_start <= d <= today)
        if out_week > 0:
            insights.append({
                "icon": "✍️", "title": "Output this week",
                "detail": "~%s output tokens in the last 7 days" % f"{out_week:,}",
                "kind": "info"})

        # --- tabs needing attention (stuck or needs), from live payload ---
        try:
            payload = build_payload_memo()
            needs = sum(1 for s in payload.get("sessions", [])
                        if s.get("status") == "needs")
            stuck = sum(1 for s in payload.get("sessions", []) if s.get("stuck"))
        except Exception:
            needs = stuck = 0
        attn = needs + stuck
        if attn > 0:
            insights.append({
                "icon": "⚠️", "title": "Needs attention",
                "detail": "%d tab%s stuck or awaiting input right now"
                % (attn, "" if attn == 1 else "s"),
                "kind": "warn"})

        # --- peak local hour over 30d ---
        if any(hourly):
            peak = max(range(24), key=lambda h: hourly[h])
            if hourly[peak] > 0:
                ampm = "am" if peak < 12 else "pm"
                h12 = peak % 12 or 12
                insights.append({
                    "icon": "🕒", "title": "Peak hour",
                    "detail": "Most active around %d%s local — %d actions over 30 days"
                    % (h12, ampm, hourly[peak]),
                    "kind": "info"})

        return {"generated": now_utc().isoformat(), "insights": insights}
    except Exception:
        return {"generated": now_utc().isoformat(), "insights": []}


# --------------------------------------------------------------------------- #
# Session actions (resume / reveal / close) — reached only via guarded POST.
# --------------------------------------------------------------------------- #

_UUID_RE = re.compile(r"^[0-9a-fA-F-]{36}$")
# Pantry idempotency keys and Arena handles (use fullmatch: "$" allows a "\n").
_RID_RE = re.compile(r"^[A-Za-z0-9_-]{16,64}$")
_QUEST_RID_RE = re.compile(r"^(quest|ach):[a-z0-9_]+:.{1,60}$")
_HANDLE_RE = re.compile(r"^[A-Za-z0-9_-]{1,64}$")

ARENA_ROOM_POSTS = (
    "/api/arena/rooms/create", "/api/arena/rooms/join",
    "/api/arena/rooms/leave", "/api/arena/rooms/rename",
    "/api/arena/rooms/password", "/api/arena/rooms/kick",
    "/api/arena/rooms/unban", "/api/arena/rooms/delete",
)


def _room_body_error(path, body):
    op = path.rsplit("/", 1)[1] if "/" in path else ""
    if op == "create":
        name = body.get("name")
        if not isinstance(name, str) or len(name) > 200 or len(name.strip()) < 1:
            return "room name required"
        pw = body.get("password")
        if not isinstance(pw, str) or len(pw) < 1 or len(pw) > 1024:
            return "password required"
        return None
    if op in ("join", "leave", "rename", "password", "kick", "unban", "delete"):
        rid = body.get("roomId")
        if not isinstance(rid, str) or not arena.ROOM_ID_RE.fullmatch(rid):
            return "roomId required"
    else:
        return "not found"
    if op == "join":
        pw = body.get("password")
        if not isinstance(pw, str) or len(pw) < 1 or len(pw) > 1024:
            return "password required"
    if op == "rename":
        name = body.get("name")
        if not isinstance(name, str) or len(name) > 200 or len(name.strip()) < 1:
            return "room name required"
    if op == "password":
        pw = body.get("password")
        if not isinstance(pw, str) or len(pw) < 1 or len(pw) > 1024:
            return "password required"
        soo = body.get("signOutOthers")
        if soo is not None and not isinstance(soo, bool):
            return "signOutOthers must be true or false"
    if op in ("kick", "unban"):
        uid = body.get("userId")
        if not isinstance(uid, str) or not arena.USER_ID_RE.fullmatch(uid):
            return "userId required"
    return None


def _room_post(path, body):
    err = _room_body_error(path, body)
    if err:
        if err == "not found":
            return 404, {"error": "not found"}
        return 400, {"error": err}
    op = path.rsplit("/", 1)[1]
    try:
        if op == "create":
            code, resp = arena.create_room(body["name"], body["password"])
        elif op == "join":
            code, resp = arena.join_room(body["roomId"], body["password"])
        elif op == "leave":
            code, resp = arena.leave_room(body["roomId"])
        elif op == "rename":
            code, resp = arena.rename_room(body["roomId"], body["name"])
        elif op == "password":
            code, resp = arena.set_room_password(
                body["roomId"], body["password"],
                sign_out_others=(body.get("signOutOthers") is True))
        elif op == "kick":
            code, resp = arena.kick_room_member(body["roomId"], body["userId"])
        elif op == "unban":
            code, resp = arena.unban_room_member(body["roomId"], body["userId"])
        elif op == "delete":
            code, resp = arena.delete_room(body["roomId"])
        else:
            return 404, {"error": "not found"}
    except Exception:
        return 500, {"error": "arena request failed"}
    return (code or 502), resp


def _cwd_for_session(sid):
    """Best-effort cwd for a session id, from the live agent list."""
    try:
        for s in build_payload_memo().get("sessions", []):
            if s.get("sessionId") == sid:
                return s.get("cwd") or ""
    except Exception:
        pass
    return ""


def _find_kitty():
    """Locate the kitty binary, or None."""
    import shutil
    for p in ("/Applications/kitty.app/Contents/MacOS/kitty",
              os.path.expanduser("~/.local/bin/kitty"),
              "/opt/homebrew/bin/kitty", "/usr/local/bin/kitty"):
        if os.path.exists(p):
            return p
    return shutil.which("kitty")


def action_resume(sid):
    """Open a new kitty window running `claude --resume <sid>` in the session's cwd
    (falls back to Terminal.app if kitty isn't installed)."""
    path = find_transcript(sid) if isinstance(sid, str) else None
    if not sid or not _UUID_RE.match(sid) or not path:
        return 400, {"error": "invalid or unknown sessionId"}
    if _is_cursor_transcript(path):
        return 400, {"error": "Cursor sessions open in Cursor"}
    cwd = _cwd_for_session(sid)
    if not cwd or not os.path.isdir(cwd):
        cwd = os.path.expanduser("~")

    # sid is a validated UUID (safe charset); cwd is passed as a list arg (no shell).
    kitty = _find_kitty()
    try:
        if kitty:
            # --single-instance opens a new OS window in the running kitty (or starts one);
            # trailing argv runs the command, then `exec zsh -l` keeps the window open.
            subprocess.Popen(
                [kitty, "--single-instance", "--directory", cwd,
                 "zsh", "-lc", "claude --resume %s; exec zsh -l" % sid],
                start_new_session=True,
                stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            # bring kitty to the front (best-effort)
            subprocess.Popen(["open", "-a", "kitty"],
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            return 200, {"ok": True, "action": "resume", "sessionId": sid, "terminal": "kitty"}
        if '"' in cwd:
            return 400, {"error": "unsupported cwd"}
        inner = "cd %s && claude --resume %s" % (shlex.quote(cwd), sid)
        script = 'tell application "Terminal" to do script "%s"' % inner.replace("\\", "\\\\")
        subprocess.run(["osascript", "-e", script,
                        "-e", 'tell application "Terminal" to activate'],
                       check=False, timeout=15, capture_output=True, text=True)
        return 200, {"ok": True, "action": "resume", "sessionId": sid, "terminal": "Terminal"}
    except Exception as e:
        return 500, {"error": "resume failed: %s" % e}


def action_reveal(sid):
    """Open the session's working directory in Finder."""
    if not sid or not _UUID_RE.match(sid) or not find_transcript(sid):
        return 400, {"error": "invalid or unknown sessionId"}
    cwd = _cwd_for_session(sid)
    if not cwd or not os.path.isdir(cwd):
        return 400, {"error": "no folder for this session"}
    try:
        subprocess.run(["open", cwd], check=False, timeout=10,
                       capture_output=True, text=True)
    except Exception as e:
        return 500, {"error": "reveal failed: %s" % e}
    return 200, {"ok": True, "action": "reveal", "sessionId": sid}


def _live_interactive_pids():
    """(pids, error): pids of the live interactive Claude sessions `claude
    agents` reports, and the CLI error (None on success)."""
    agents, err = get_live_agents()
    out = set()
    for a in agents or []:
        if not isinstance(a, dict):
            continue
        if (a.get("kind") or "interactive") != "interactive":
            continue
        p = a.get("pid")
        if isinstance(p, int) and not isinstance(p, bool):
            out.add(p)
    return out, err


def action_close(pid):
    """Terminate an interactive Claude session by pid. The pid must be one of
    the live interactive agents (never this server, init, or an arbitrary
    process), and its command line must still look like claude."""
    if isinstance(pid, bool):
        return 400, {"error": "pid must be an integer"}
    try:
        pid = int(pid)
    except Exception:
        return 400, {"error": "pid must be an integer"}
    if pid <= 1 or pid == os.getpid():
        return 400, {"error": "refusing that pid"}
    live, err = _live_interactive_pids()
    if err:
        # Fail closed, but say why: we could not list sessions at all.
        return 503, {"error": "cannot verify live sessions: %s" % err}
    if pid not in live:
        return 400, {"error": "pid %d is not a live interactive Claude session" % pid}
    try:
        ps = subprocess.run(["ps", "-p", str(pid), "-o", "command="],
                            capture_output=True, text=True, timeout=10)
    except Exception as e:
        return 500, {"error": "ps failed: %s" % e}
    cmd = (ps.stdout or "").strip().lower()
    if not cmd or "claude" not in cmd:
        return 400, {"error": "pid %d is not a Claude process" % pid}
    try:
        os.kill(pid, signal.SIGTERM)
    except ProcessLookupError:
        return 400, {"error": "no such process"}
    except Exception as e:
        return 500, {"error": "close failed: %s" % e}
    return 200, {"ok": True, "action": "close", "pid": pid}


# --------------------------------------------------------------------------- #
# launchd auto-start (CLI only — never reachable over HTTP)
# --------------------------------------------------------------------------- #

def render_plist(port=None):
    port = port if port is not None else SERVER_PORT
    py = sys.executable or "python3"
    script = os.path.abspath(__file__)
    log = os.path.join(HERE, "claude-hq.log")
    err = os.path.join(HERE, "claude-hq.err.log")
    return (
        '<?xml version="1.0" encoding="UTF-8"?>\n'
        '<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" '
        '"http://www.apple.com/DTDs/PropertyList-1.0.dtd">\n'
        '<plist version="1.0">\n'
        '<dict>\n'
        '  <key>Label</key>\n  <string>%s</string>\n'
        '  <key>ProgramArguments</key>\n  <array>\n'
        '    <string>%s</string>\n    <string>%s</string>\n'
        '    <string>--port</string>\n    <string>%d</string>\n'
        '    <string>--no-open</string>\n  </array>\n'
        '  <key>EnvironmentVariables</key>\n  <dict>\n'
        '    <key>PATH</key>\n    <string>%s</string>\n  </dict>\n'
        '  <key>RunAtLoad</key>\n  <true/>\n'
        '  <key>KeepAlive</key>\n  <true/>\n'
        '  <key>StandardOutPath</key>\n  <string>%s</string>\n'
        '  <key>StandardErrorPath</key>\n  <string>%s</string>\n'
        '</dict>\n</plist>\n'
        % (LAUNCH_LABEL, py, script, port,
           "%s:/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin"
           % os.path.expanduser("~/.local/bin"), log, err)
    )


def install_launchagent(port):
    os.makedirs(os.path.dirname(LAUNCH_PLIST), exist_ok=True)
    with open(LAUNCH_PLIST, "w", encoding="utf-8") as f:
        f.write(render_plist(port))
    subprocess.run(["launchctl", "unload", LAUNCH_PLIST],
                   capture_output=True, text=True)
    r = subprocess.run(["launchctl", "load", LAUNCH_PLIST],
                       capture_output=True, text=True)
    print("Installed LaunchAgent: %s" % LAUNCH_PLIST)
    print("Claude HQ will now start at login on http://127.0.0.1:%d" % port)
    if r.returncode != 0 and r.stderr.strip():
        print("launchctl load said: %s" % r.stderr.strip())


def uninstall_launchagent():
    if os.path.exists(LAUNCH_PLIST):
        subprocess.run(["launchctl", "unload", LAUNCH_PLIST],
                       capture_output=True, text=True)
        os.remove(LAUNCH_PLIST)
        print("Removed LaunchAgent: %s" % LAUNCH_PLIST)
    else:
        print("No LaunchAgent installed (%s not found)." % LAUNCH_PLIST)


# --------------------------------------------------------------------------- #
# Server-persisted config + per-session meta (local JSON files in HERE).
# --------------------------------------------------------------------------- #

CONFIG_PATH = os.path.join(HERE, "config.json")
META_PATH = os.path.join(HERE, "sessions-meta.json")

KNOWN_THEMES = ("aurora", "midnight", "forest", "mono")
KNOWN_CREATURE_PACKS = ("monsters", "pokemon", "pokemon3d", "aniimo", "village", "animals", "faces")
# Per-cell upper index (inclusive) of the trainer-avatar spec
# [skin,hair,hairColor,outfit,outfitColor,hat,accessory,bg,face]. MUST stay
# byte-identical to TR_MAX in ui/app/06-trainer-packs.js, which
# tests/test_trainer_spec.py checks. The avatar never leaves this machine --
# schemas.py has no copy, whatever an older comment here said.
# Append-only: raising a max never renumbers existing choices, so a saved
# avatar keeps meaning what it meant.
TRAINER_MAX = (6, 8, 8, 8, 8, 7, 5, 8, 4)
# The 3D character (HQ 2.4): per-axis upper index (inclusive) of
# [base, skin, hairColor, outfitColor, hairStyle, headwear, accessory, eyes,
#  background, outfitStyle]. MUST stay identical to MAX in games/avatar3d.js
# (tests/test_character.py checks). Append-only, like TRAINER_MAX.
CHARACTER_MAX = (5, 7, 9, 9, 2, 5, 3, 1, 4, 1)
DEFAULT_CONFIG = {
    "theme": "aurora",
    "creaturePack": "pokemon3d",
    "refreshMs": 5000,
    "stuckMinutes": 15,
    "dailyBudgetUSD": 0,
    "trainerName": "",
    # None = auto-derive from a stable handle (never []/{}: a mutable default
    # would alias across the shallow dict(base) copy in _validate_config).
    "trainerAvatar": None,
    # None = not built yet: the page derives one from trainerAvatar (once).
    "character": None,
    "arenaUrl": "",
    "arenaEnabled": False,
    "arenaShareCost": False,
    "creatureFatigue": True,
    # Share what you're playing (Spotify / Apple Music / YouTube Music) with the
    # Arena while paired. On by default; Settings and the Music view turn it off.
    "musicShare": True,
    # Whose YouTube login yt-dlp may use for room songs ("" = none); see music.COOKIE_BROWSERS.
    "musicCookies": "",
    # HQ 2.5 work signals (skills, loot): consent required, so both start off.
    "workSignals": False,
    "workSignalsPRs": False,
}

_config_lock = threading.Lock()
_meta_lock = threading.Lock()


def _validate_config(raw, base=None):
    """Return a clean config dict: only known keys, validated types/ranges.
    Invalid/unknown values fall back to `base` (defaults, or current config)."""
    cfg = dict(base if base is not None else DEFAULT_CONFIG)
    if not isinstance(raw, dict):
        return cfg
    if raw.get("theme") in KNOWN_THEMES:
        cfg["theme"] = raw["theme"]
    if raw.get("creaturePack") in KNOWN_CREATURE_PACKS:
        cfg["creaturePack"] = raw["creaturePack"]
    try:
        rm = int(raw.get("refreshMs"))
        if 1000 <= rm <= 60000:
            cfg["refreshMs"] = rm
    except Exception:
        pass
    try:
        sm = int(raw.get("stuckMinutes"))
        if 1 <= sm <= 240:
            cfg["stuckMinutes"] = sm
    except Exception:
        pass
    try:
        db = float(raw.get("dailyBudgetUSD"))
        if db >= 0:
            cfg["dailyBudgetUSD"] = db
    except Exception:
        pass
    tn = raw.get("trainerName")
    if isinstance(tn, str):
        # printable chars only, whitespace collapsed, capped; "" = auto-derive
        tn = "".join(ch for ch in tn if ch.isprintable())
        cfg["trainerName"] = " ".join(tn.split())[:32]
    if "trainerAvatar" in raw:
        # A tiny fixed-length list of small ints (user menu choices only). None
        # keeps the auto-derive sentinel; a list is coerced to exactly 9 cells,
        # each clamped to [0, TRAINER_MAX[i]]; anything else is ignored.
        ta = raw.get("trainerAvatar")
        if ta is None:
            cfg["trainerAvatar"] = None
        elif isinstance(ta, list):
            spec = []
            for i, m in enumerate(TRAINER_MAX):
                try:
                    v = int(ta[i]) if i < len(ta) else 0
                except Exception:
                    v = 0
                spec.append(v % (m + 1) if v >= 0 else ((v % (m + 1)) + (m + 1)) % (m + 1))
            cfg["trainerAvatar"] = spec
    if "character" in raw:
        # Same rule as trainerAvatar: exactly len(CHARACTER_MAX) small ints, each
        # wrapped into range; anything that isn't a list is ignored; None resets.
        ch = raw.get("character")
        if ch is None:
            cfg["character"] = None
        elif isinstance(ch, list):
            spec = []
            for i, m in enumerate(CHARACTER_MAX):
                try:
                    v = int(ch[i]) if i < len(ch) else 0
                except Exception:
                    v = 0
                spec.append(v % (m + 1))
            cfg["character"] = spec
    au = raw.get("arenaUrl")
    if isinstance(au, str):
        au = au.strip()
        # http/https only: this string becomes an outbound request target.
        cfg["arenaUrl"] = au[:256] if au.startswith(("http://", "https://")) else ""
    if "musicCookies" in raw and (raw.get("musicCookies") == "" or raw.get("musicCookies") in music.COOKIE_BROWSERS):
        cfg["musicCookies"] = raw.get("musicCookies")
    for key in ("arenaEnabled", "arenaShareCost", "creatureFatigue", "musicShare"):
        if key in raw:
            cfg[key] = bool(raw.get(key))
    for key in ("workSignals", "workSignalsPRs"):
        if isinstance(raw.get(key), bool):
            cfg[key] = raw[key]
    return cfg


def load_config():
    """Read + validate config.json; returns defaults if missing/corrupt (a
    corrupt file is quarantined, see _load_json_guarded)."""
    return _validate_config(_load_json_guarded(CONFIG_PATH, {}))


def save_config(patch):
    """Merge `patch` into the current config, validate, persist, return saved.
    Raises OSError if the file could not be written."""
    with _config_lock:
        cur = load_config()
        cfg = _validate_config(patch, base=cur)
        _atomic_write_json(CONFIG_PATH, cfg)
        return cfg


def _clean_meta_entry(entry):
    """Normalise one session-meta entry: pinned bool, tags list, note capped."""
    if not isinstance(entry, dict):
        entry = {}
    pinned = bool(entry.get("pinned"))
    tags = []
    raw_tags = entry.get("tags")
    if isinstance(raw_tags, list):
        for t in raw_tags:
            if not isinstance(t, str):
                continue
            t = t.strip()[:24]
            if t:
                tags.append(t)
            if len(tags) >= 12:
                break
    note = entry.get("note")
    note = note[:2000] if isinstance(note, str) else ""
    name = entry.get("name")
    name = name.strip()[:80] if isinstance(name, str) else ""
    return {"pinned": pinned, "tags": tags, "note": note, "name": name}


def load_meta():
    """Read sessions-meta.json -> {sessionId: {pinned,tags,note}} (validated)."""
    raw = _load_json_guarded(META_PATH, {})
    out = {}
    if isinstance(raw, dict):
        for sid, entry in raw.items():
            if isinstance(sid, str) and _UUID_RE.match(sid):
                out[sid] = _clean_meta_entry(entry)
    return out


def save_meta(sid, patch):
    """Merge `patch` into the meta entry for `sid`, persist, return the entry."""
    with _meta_lock:
        data = load_meta()
        cur = data.get(sid, {"pinned": False, "tags": [], "note": "", "name": ""})
        merged = dict(cur)
        if isinstance(patch, dict):
            if "pinned" in patch:
                merged["pinned"] = patch["pinned"]
            if "tags" in patch:
                merged["tags"] = patch["tags"]
            if "note" in patch:
                merged["note"] = patch["note"]
            if "name" in patch:
                merged["name"] = patch["name"]
        entry = _clean_meta_entry(merged)
        data[sid] = entry
        _atomic_write_json(META_PATH, data)  # OSError -> caller (HTTP 500)
        return entry


# --------------------------------------------------------------------------- #
# Evolution picks (which branch / Mega form a session's Pokemon becomes)
#
# The page used to keep these only in its own localStorage (`hq_evo_choice`), so
# another browser or the notch never saw them. Now the page also POSTs each pick
# here; they live in evo-choices.json (0600, git-ignored, never leaves the
# machine) and ride in /api/sessions as `creature.evo`. The allowed values are
# read from ui/app/06-trainer-packs.js itself (POKE_EVO / POKE_BRANCH /
# MEGA_FORMS), so the server and the page cannot disagree on what is valid.
# --------------------------------------------------------------------------- #

EVO_PATH = os.path.join(HERE, "evo-choices.json")
EVO_MAX = 2000                       # sessions kept (the oldest picks drop beyond this)
_evo_lock = threading.Lock()
_evo_tables_cache = None


def evo_tables():
    """({species: set(final dexes it may become)}, {final dex: set(mega slugs)}),
    parsed once from ui/app/06-trainer-packs.js."""
    global _evo_tables_cache
    if _evo_tables_cache is not None:
        return _evo_tables_cache
    finals, megas = {}, {}
    try:
        with open(os.path.join(HERE, "ui", "app", "06-trainer-packs.js"), encoding="utf-8") as f:
            js = f.read()

        def var(name):
            m = re.search(r"var %s=(.*?);\n" % name, js, re.S)
            return m.group(1) if m else ""
        for i, line in enumerate(json.loads(var("POKE_EVO") or "[]")):
            if isinstance(line, list) and line:
                finals[i] = {int(line[-1])}
        for idx, body in re.findall(r"(\d+):\[(\{.*?\})\]", var("POKE_BRANCH")):
            finals.setdefault(int(idx), set()).update(int(d) for d in re.findall(r"dex:(\d+)", body))
        for key, body in re.findall(r"(\d+):\[(\{name:.*?\})\]", var("MEGA_FORMS")):
            megas[int(key)] = set(re.findall(r'slug:"([a-z0-9-]+)"', body))
    except (OSError, ValueError):
        pass
    _evo_tables_cache = (finals, megas)
    return _evo_tables_cache


def clean_evo_choice(sid, entry):
    """This session's pick, keeping only what is valid for its species:
    {branchDex?, megaSlug?}; {} when nothing in it is."""
    if not isinstance(entry, dict) or not (isinstance(sid, str) and _UUID_RE.fullmatch(sid)):
        return {}
    finals, megas = evo_tables()
    allowed = finals.get(_session_hash(sid) % 48, set())
    out = {}
    b = entry.get("branchDex")
    if isinstance(b, int) and not isinstance(b, bool) and b in allowed and len(allowed) > 1:
        out["branchDex"] = b
    # a Mega must belong to the chosen final, or (no branch picked) to any final it may become
    finals_now = [out["branchDex"]] if "branchDex" in out else sorted(allowed)
    pool = set()
    for d in finals_now:
        pool |= megas.get(d, set())
    m = entry.get("megaSlug")
    if isinstance(m, str) and m in pool:
        out["megaSlug"] = m
    return out


def merge_evo_choices(sessions, evo):
    """Put each session's pick on its creature as `creature.evo` (only where one exists,
    so every other session's payload is unchanged)."""
    for s in sessions or []:
        sid = s.get("sessionId") if isinstance(s, dict) else None
        if sid in evo and isinstance(s.get("creature"), dict):
            s["creature"]["evo"] = dict(evo[sid])
    return sessions


def load_evo_choices():
    """evo-choices.json -> {sessionId: {branchDex?, megaSlug?}} (validated)."""
    raw = _load_json_guarded(EVO_PATH, {})
    out = {}
    if isinstance(raw, dict):
        for sid, entry in raw.items():
            e = clean_evo_choice(sid, entry)
            if e:
                out[sid] = e
    return out


def save_evo_choice(sid, patch):
    """Merge a pick into this session's entry (a null field clears it), persist,
    return the entry. ValueError for a bad session id or a pick that is not
    valid for this session's Pokemon (nothing is written then)."""
    if not (isinstance(sid, str) and _UUID_RE.fullmatch(sid)):
        raise ValueError("invalid sessionId")
    if not isinstance(patch, dict):
        raise ValueError("bad pick")
    with _evo_lock:
        data = load_evo_choices()
        cur = dict(data.get(sid, {}))
        for k in ("branchDex", "megaSlug"):
            if k in patch:
                if patch[k] is None:
                    cur.pop(k, None)
                else:
                    cur[k] = patch[k]
        entry = clean_evo_choice(sid, cur)
        for k in ("branchDex", "megaSlug"):
            if patch.get(k) is not None and entry.get(k) != patch.get(k):
                raise ValueError("%s is not a valid pick for this session's Pokemon" % k)
        data.pop(sid, None)
        if entry:
            data[sid] = entry               # newest last, so a trim keeps it
        if len(data) > EVO_MAX:
            data = dict(list(data.items())[-EVO_MAX:])
        _atomic_write_json(EVO_PATH, data)  # OSError -> caller (HTTP 500)
        return entry


# --------------------------------------------------------------------------- #
# Local meal ledger (which session ate what; never leaves this machine)
#
# Written ONLY by pantry_eat, after the Arena confirmed the snack, and deduped
# globally by requestId: a replayed or reused requestId can never add a second
# meal. The effect comes from FOOD_EFFECTS by kind and is never stored.
# --------------------------------------------------------------------------- #

MEALS_PATH = os.path.join(HERE, "meals.json")
MEALS_KEEP_SECS = 172800
MEALS_MAX = 500
MEALS_PER_SID = 20

_meals_lock = threading.Lock()
_EAT_INFLIGHT = set()  # requestIds with an eat in flight (guarded by _meals_lock)


def _read_meal_entries(now):
    """Valid meal entries from meals.json; a missing or corrupt file is []."""
    try:
        with open(MEALS_PATH, "r", encoding="utf-8") as f:
            raw = json.load(f)
    except Exception:
        return []
    items = raw.get("meals") if isinstance(raw, dict) else None
    if not isinstance(items, list):
        return []
    out = []
    for m in items:
        if not isinstance(m, dict):
            continue
        sid, kind, rid, at = m.get("sessionId"), m.get("kind"), m.get("requestId"), m.get("at")
        if not (isinstance(sid, str) and _UUID_RE.fullmatch(sid)):
            continue
        if not (isinstance(kind, str) and kind in FOOD_EFFECTS):
            continue
        if not (isinstance(rid, str) and _RID_RE.fullmatch(rid)):
            continue
        if isinstance(at, bool) or not isinstance(at, (int, float)) or not math.isfinite(at):
            continue
        if not (now - MEALS_KEEP_SECS <= at <= now + 60):
            continue
        out.append({"requestId": rid, "sessionId": sid, "kind": kind, "at": float(at)})
    return out


def load_meals(now=None):
    """{sessionId: [(at, kind), ...]} sorted by time, for the fatigue walk."""
    out = {}
    for m in _read_meal_entries(time.time() if now is None else now):
        out.setdefault(m["sessionId"], []).append((m["at"], m["kind"]))
    for v in out.values():
        v.sort()
    return out


def find_meal(rid):
    """The recorded meal for this requestId, or None."""
    for m in _read_meal_entries(time.time()):
        if m["requestId"] == rid:
            return m
    return None


def _write_meals(entries):
    """Atomic write: temp file in the same folder, fsync, then rename over."""
    tmp = tempfile.NamedTemporaryFile("w", encoding="utf-8",
                                      dir=os.path.dirname(MEALS_PATH),
                                      prefix=".meals-", suffix=".tmp", delete=False)
    try:
        with tmp:
            json.dump({"version": 1, "meals": entries}, tmp)
            tmp.flush()
            os.fsync(tmp.fileno())
        os.chmod(tmp.name, 0o600)
        os.replace(tmp.name, MEALS_PATH)
    except BaseException:
        try:
            os.unlink(tmp.name)
        except OSError:
            pass
        raise


def record_meal(rid, sid, kind, at, now=None):
    """Record one meal and return its entry. Idempotent by requestId across ALL
    sessions: a known rid returns the first entry unchanged. Raises if the file
    can't be written (pantry_eat turns that into a retryable 202)."""
    with _meals_lock:
        now = time.time() if now is None else now
        entries = _read_meal_entries(now)
        for m in entries:
            if m["requestId"] == rid:
                return m
        entry = {"requestId": rid, "sessionId": sid, "kind": kind, "at": float(at)}
        entries.append(entry)
        # prune: the keep window, then the newest MEALS_PER_SID per session and
        # the newest MEALS_MAX overall. A meal inside the fatigue window is never
        # dropped for the per-session count: the walk replays every one of them,
        # so losing an old Revive Tonic or Bento would make the snack just eaten
        # leave the creature worse off. MEALS_MAX stays the hard bound: it is
        # above the most eats the Arena allows in any 24 h (MAX_OPS_PER_DAY is
        # 200 per UTC day and every eat is one op, so at most 400).
        fz_lo = now - FATIGUE_WINDOW_SECS
        kept, per_sid = [], {}
        for m in sorted(entries, key=lambda m: -m["at"]):
            if not (now - MEALS_KEEP_SECS <= m["at"] <= now + 60):
                continue
            n = per_sid.get(m["sessionId"], 0)
            if n >= MEALS_PER_SID and m["at"] < fz_lo:
                continue
            per_sid[m["sessionId"]] = n + 1
            kept.append(m)
            if len(kept) >= MEALS_MAX:
                break
        kept.reverse()
        _write_meals(kept)
        return entry


# --------------------------------------------------------------------------- #
# Arena pantry proxy (Poke Coins, food, gifts). The page talks only to these
# local routes; arena.pantry() forwards an allowlisted body to the server.
# --------------------------------------------------------------------------- #

def _int_in(v, lo, hi):
    return isinstance(v, int) and not isinstance(v, bool) and lo <= v <= hi


def _food_kind(v):
    return isinstance(v, str) and v in FOOD_EFFECTS


_PROC_STARTED = time.time()


def _ps_self(pid=None):
    """(cpu %, rss MB) of a process from ps (macOS and Linux), or (None, None)."""
    try:
        out = subprocess.run(["ps", "-o", "%cpu=,rss=", "-p", str(pid or os.getpid())],
                             capture_output=True, text=True, timeout=3).stdout.split()
        return float(out[0]), round(float(out[1]) / 1024.0, 1)
    except (OSError, ValueError, IndexError, subprocess.SubprocessError):
        return None, None


def _machine_ram_mb():
    try:
        if sys.platform == "darwin":
            return round(int(subprocess.run(["sysctl", "-n", "hw.memsize"], capture_output=True, text=True,
                                            timeout=3).stdout.strip()) / 1048576)
        with open("/proc/meminfo") as f:
            for line in f:
                if line.startswith("MemTotal:"):
                    return round(int(line.split()[1]) / 1024)
    except (OSError, ValueError, subprocess.SubprocessError):
        pass
    return None


def server_stats():
    """This HQ server's own numbers, for Settings: memory, CPU, uptime, threads, version."""
    cpu, rss = _ps_self()
    try:
        load = [round(x, 2) for x in os.getloadavg()]
    except OSError:
        load = None
    return {"version": APP_VERSION, "python": sys.version.split()[0], "pid": os.getpid(), "boot": BOOT_ID,
            "uptimeSecs": int(time.time() - _PROC_STARTED), "cpuPct": cpu, "rssMb": rss,
            "machineRamMb": _machine_ram_mb(), "cpus": os.cpu_count(), "load": load,
            "threads": threading.active_count(), "platform": sys.platform}


def hq_crew_counts():
    """How many sessions are working / need you / are idle right now (HQ 2.1).
    The only thing about your sessions an open HQ shares: three counts."""
    out = {"working": 0, "needs": 0, "idle": 0}
    try:
        for s in (build_payload_memo().get("sessions") or []):
            st = s.get("status")
            if st in out:
                out[st] += 1
    except Exception:
        pass
    return out


def pantry_body(action, body):
    """Validate a page request -> (clean body for arena.pantry, None) or
    (None, error text). `clean` never contains a sessionId."""
    if action not in arena.PANTRY_ACTIONS:
        return None, "unknown pantry action"
    if action == "claim":
        return {}, None
    body = body if isinstance(body, dict) else {}
    rid = body.get("requestId")
    if not (isinstance(rid, str) and _RID_RE.fullmatch(rid)):
        return None, "invalid requestId"
    kind = body.get("kind")
    if action == "eat":
        if not _food_kind(kind):
            return None, "unknown food"
        return {"requestId": rid, "kind": kind}, None
    if action == "buy":
        if not _food_kind(kind):
            return None, "unknown food"
        qty = body.get("qty", 1)
        if not _int_in(qty, 1, 5):
            return None, "qty must be a whole number from 1 to 5"
        return {"requestId": rid, "kind": kind, "qty": qty}, None
    # give
    to = body.get("toHandle")
    to = to.strip() if isinstance(to, str) else ""
    if not _HANDLE_RE.fullmatch(to):
        return None, "invalid handle"
    coins = body.get("coins", 0)
    if not _int_in(coins, 0, 5):
        return None, "coins must be a whole number from 0 to 5"
    qty = body.get("qty", 0)
    if not _int_in(qty, 0, 3):
        return None, "qty must be a whole number from 0 to 3"
    if kind is not None and not _food_kind(kind):
        return None, "unknown food"
    if qty > 0 and kind is None:
        return None, "pick a food for that amount"
    if kind is not None and qty == 0:
        return None, "pick an amount for that food"
    if coins == 0 and qty == 0:
        return None, "a gift needs Poke Coins or food"
    note = body.get("note")
    note = " ".join("".join(c for c in note if c.isprintable()).split())[:80] \
        if isinstance(note, str) else ""
    clean = {"requestId": rid, "toHandle": to, "coins": coins, "qty": qty, "note": note}
    if kind is not None:
        clean["kind"] = kind
    return clean, None


_QUEST_KINDS = ("quest", "achievement")
_QUEST_TIERS = ("bronze", "silver", "gold")


def _quest_reward(body):
    """Validate + forward a quest/achievement reward claim to the Arena."""
    body = body if isinstance(body, dict) else {}
    rid = body.get("requestId")
    if not (isinstance(rid, str) and _QUEST_RID_RE.fullmatch(rid)):
        return 400, {"error": "invalid requestId"}
    kind = body.get("kind")
    if kind not in _QUEST_KINDS:
        return 400, {"error": "kind must be quest or achievement"}
    quest_id = body.get("questId")
    if not (isinstance(quest_id, str) and 1 <= len(quest_id) <= 40):
        return 400, {"error": "invalid questId"}
    tier = body.get("tier")
    if tier is not None and tier not in _QUEST_TIERS:
        return 400, {"error": "invalid tier"}
    coins = body.get("coins")
    if not _int_in(coins, 1, 15):
        return 400, {"error": "coins must be 1-15"}
    code, resp = arena.quest_reward(rid, kind, quest_id, tier, coins)
    return (code or 502), resp


def _overlay_food_effects(resp):
    """Stamp the LOCAL effect of each food onto a server catalog, so the page
    always previews the effect that will actually apply here."""
    if not isinstance(resp, dict) or not isinstance(resp.get("catalog"), list):
        return resp
    catalog = []
    for item in resp["catalog"]:
        kind = item.get("kind") if isinstance(item, dict) else None
        if not _food_kind(kind):
            continue
        secs, revives = FOOD_EFFECTS[kind]
        catalog.append(dict(item, restoreMins=secs // 60, revives=revives,
                            wakeToMins=max(0, FATIGUE_REVIVE_TO_SECS - secs) // 60
                            if revives else None))
    return dict(resp, catalog=catalog)


def _meal_view(kind, at):
    return {"kind": kind,
            "at": datetime.fromtimestamp(at, tz=timezone.utc).isoformat(),
            "restoreMins": FOOD_EFFECTS[kind][0] // 60,
            "revives": FOOD_EFFECTS[kind][1]}


def _payload_session(sid):
    return next((s for s in build_payload_memo().get("sessions", [])
                 if s.get("sessionId") == sid), None)


def _eat_target(sid):
    """The session a snack is for: its payload entry, else (for a transcript the
    drawer opened outside the payload) a stub carrying the same creature.fatigue
    build_session_detail showed. None if no transcript has that id."""
    sess = _payload_session(sid)
    if sess is not None:
        return sess
    agg = scan_file(find_transcript(sid))
    if agg is None:
        return None
    fz = _offpayload_fatigue(sid, agg)
    return {"sessionId": sid, "creature": {"fatigue": fz} if fz is not None else {}}


def pantry_eat(body):
    """Feed one session's creature -> (code, dict).

    The local state gate only protects the user's own pantry; the Arena still
    has to confirm the snack, and the meal is stamped at the server's time. A
    retry (same requestId, retry: true) skips the gate."""
    body = body if isinstance(body, dict) else {}
    sid = body.get("sessionId")
    if not (isinstance(sid, str) and _UUID_RE.fullmatch(sid)):
        return 400, {"error": "invalid sessionId"}
    clean, err = pantry_body("eat", body)
    if err:
        return 400, {"error": err}
    rid, kind = clean["requestId"], clean["kind"]
    retry = body.get("retry") is True

    m = find_meal(rid)
    if m is not None:
        if m["sessionId"] == sid and m["kind"] == kind:
            sess = _payload_session(sid) or {"sessionId": sid}
            return 200, {"op": "eat", "replayed": True, "kind": kind, "sessionId": sid,
                         "meal": _meal_view(kind, m["at"]),
                         "fatigue": session_fatigue_now(sess)}
        return 409, {"error": "that requestId was already used for a different meal",
                     "code": "rid_reused"}

    with _meals_lock:
        if rid in _EAT_INFLIGHT:
            return 202, {"pending": True, "requestId": rid}
        _EAT_INFLIGHT.add(rid)
    try:
        sess = _eat_target(sid)
        if sess is None:
            return 404, {"error": "unknown session"}
        fz = (sess.get("creature") or {}).get("fatigue")
        if fz is None:
            return 409, {"error": "Creature energy is turned off in Settings",
                         "code": "fatigue_off"}
        if not retry:
            state = fz.get("state")
            if state == "rested":
                return 409, {"error": "This creature is full of energy: no snack needed",
                             "code": "not_hungry"}
            revives = FOOD_EFFECTS[kind][1]
            if state == "unconscious" and not revives:
                return 409, {"error": "This creature has fainted: only a revive item like "
                                      "a Revive Tonic (or a break) can wake it", "code": "fainted"}
            if revives and state != "unconscious":
                return 409, {"error": "Revive items only work on a fainted creature",
                             "code": "not_fainted"}

        code, resp = arena.pantry("eat", {"requestId": rid, "kind": kind})
        if code == 200 and isinstance(resp, dict) and resp.get("kind") == kind:
            # The server's ORIGINAL time (a replay returns it too), never in the future.
            now = time.time()
            at = min(_epoch(resp.get("at")) or now, now)
            try:
                entry = record_meal(rid, sid, kind, at)
            except Exception:
                return 202, {"pending": True, "requestId": rid,
                             "error": "Ate it, but couldn't save the meal on this "
                                      "machine. Retrying…"}
            if entry["sessionId"] != sid or entry["kind"] != kind:
                # lost a race with the same requestId used for another meal
                return 409, {"error": "that requestId was already used for a different meal",
                             "code": "rid_reused"}
            _invalidate_payload_memo()
            return 200, dict(_overlay_food_effects(resp), sessionId=sid,
                             meal=_meal_view(kind, entry["at"]),
                             fatigue=session_fatigue_now(sess))
        if code == 0 or code >= 500:
            return 202, {"pending": True, "requestId": rid,
                         "error": "The Arena didn't answer. Your snack is safe: "
                                  "Claude HQ will retry it."}
        if code == 200:
            return 502, {"error": "the Arena sent an unexpected answer"}
        return code, resp
    finally:
        with _meals_lock:
            _EAT_INFLIGHT.discard(rid)


# --------------------------------------------------------------------------- #
# Full-conversation transcript events (paged view + markdown export).
# Cached per (path, mtime, size), like scan_file / search entries.
# --------------------------------------------------------------------------- #

_transcript_cache = {}
_transcript_lock = threading.Lock()


def _iter_transcript_events(path):
    """Read one transcript into an ordered list of conversation events:
    {i, t(iso), role in you|claude|tool|system, text(<=1200), tool(str|None)}."""
    if _is_cursor_transcript(path):
        return _iter_cursor_events(path)
    events = []
    try:
        f = open(path, "r", encoding="utf-8", errors="replace")
    except Exception:
        return events
    i = 0
    with f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            if not isinstance(o, dict):
                continue
            try:
                typ = o.get("type")
                ts = parse_ts(o.get("timestamp"))
                tiso = ts.isoformat() if ts else ""
                if typ == "user":
                    content = (o.get("message") or {}).get("content")
                    if is_real_human_prompt(content):
                        events.append({
                            "i": i, "t": tiso, "role": "you",
                            "text": truncate(clean_prompt(content), 1200),
                            "tool": None,
                        })
                        i += 1
                elif typ == "assistant":
                    blocks = (o.get("message") or {}).get("content")
                    if isinstance(blocks, list):
                        texts = []
                        for b in blocks:
                            if not isinstance(b, dict):
                                continue
                            bt = b.get("type")
                            if bt == "text":
                                txt = b.get("text") or ""
                                if txt.strip():
                                    texts.append(txt)
                            elif bt == "tool_use":
                                if texts:
                                    events.append({
                                        "i": i, "t": tiso, "role": "claude",
                                        "text": truncate(
                                            strip_markdown("\n".join(texts)), 1200),
                                        "tool": None,
                                    })
                                    i += 1
                                    texts = []
                                name = b.get("name") or "Tool"
                                events.append({
                                    "i": i, "t": tiso, "role": "tool",
                                    "text": truncate(tool_label(b) or name, 1200),
                                    "tool": name,
                                })
                                i += 1
                        if texts:
                            events.append({
                                "i": i, "t": tiso, "role": "claude",
                                "text": truncate(
                                    strip_markdown("\n".join(texts)), 1200),
                                "tool": None,
                            })
                            i += 1
                elif typ == "system":
                    # Same rule as the session's "needs" status: only a final
                    # system api_error record is an error event.
                    if _record_error_sig(o):
                        content = o.get("content")
                        if not (isinstance(content, str) and content.strip()):
                            err = o.get("error")
                            if isinstance(err, dict) and isinstance(
                                    err.get("message"), str):
                                content = err["message"]
                            elif isinstance(err, str):
                                content = err
                            elif err is not None:
                                content = json.dumps(err, default=str)
                            else:
                                content = "API error"
                        events.append({
                            "i": i, "t": tiso, "role": "system",
                            "text": truncate(strip_markdown(content), 1200),
                            "tool": None,
                        })
                        i += 1
            except Exception:
                continue
    return events


def _iter_cursor_events(path):
    """Cursor agent transcript -> the same event list Claude transcripts produce."""
    events = []
    turn_ts = None
    pending = []

    def add(ts, role, text, tool=None):
        events.append({
            "i": len(events),
            "t": ts.isoformat() if ts else "",
            "role": role,
            "text": truncate(text or "", 1200),
            "tool": tool,
        })

    def emit(ts, role, text, tool=None):
        nonlocal turn_ts
        if ts is not None and (turn_ts is None or ts >= turn_ts):
            if turn_ts is None and pending:
                queued = pending[:]
                pending.clear()
                turn_ts = ts
                for role0, text0, tool0 in queued:
                    add(ts, role0, text0, tool0)
            turn_ts = ts
        use = ts or turn_ts
        if use is None:
            pending.append((role, text, tool))
        else:
            add(use, role, text, tool)

    try:
        f = open(path, "r", encoding="utf-8", errors="replace")
    except Exception:
        return events
    with f:
        for line in f:
            line = line.strip()
            if not line:
                continue
            try:
                o = json.loads(line)
            except Exception:
                continue
            if not isinstance(o, dict):
                continue
            try:
                role = o.get("role")
                if role == "user":
                    text = _cursor_message_text(o)
                    query = _cursor_user_query(text)
                    if query:
                        emit(_parse_cursor_clock(text), "you", query)
                elif role == "assistant":
                    blocks = (o.get("message") or {}).get("content")
                    if isinstance(blocks, str):
                        blocks = [{"type": "text", "text": blocks}]
                    if not isinstance(blocks, list):
                        continue
                    texts = []
                    for b in blocks:
                        if not isinstance(b, dict):
                            continue
                        if b.get("type") == "text" and (b.get("text") or "").strip():
                            texts.append(b.get("text") or "")
                        elif b.get("type") == "tool_use":
                            if texts:
                                emit(None, "claude", strip_markdown("\n".join(texts)))
                                texts = []
                            name = b.get("name") or "Tool"
                            emit(None, "tool", tool_label(b) or name, name)
                    if texts:
                        emit(None, "claude", strip_markdown("\n".join(texts)))
            except Exception:
                continue
    if pending:
        try:
            mt = datetime.fromtimestamp(os.path.getmtime(path), tz=timezone.utc)
        except Exception:
            mt = None
        if mt is not None:
            for role0, text0, tool0 in pending:
                add(mt, role0, text0, tool0)
    return events


def get_transcript_events(path):
    """Cached ordered events for one transcript, rebuilt on (mtime,size) change."""
    if not path:
        return []
    try:
        st = os.stat(path)
    except Exception:
        return []
    key = (st.st_mtime, st.st_size)
    with _transcript_lock:
        c = _transcript_cache.get(path)
        if c is not None and c.get("_key") == key:
            return c["events"]
    events = _iter_transcript_events(path)
    with _transcript_lock:
        _transcript_cache[path] = {"_key": key, "events": events}
    return events


def build_session_markdown(sid, path):
    """Render a whole session as a readable Markdown transcript."""
    agg = scan_file(path)
    events = get_transcript_events(path)
    title = session_heading(agg if isinstance(agg, dict) else {}, sid)
    folder = _pretty_folder(agg.get("folder")) if isinstance(agg, dict) else "~"
    who = "Cursor" if isinstance(agg, dict) and agg.get("source") == "cursor" else "Claude"
    ts_list = [e["t"] for e in events if e.get("t")]
    lines = ["# %s" % title, "",
             "- Folder: %s" % folder,
             "- Source: %s" % who,
             "- Session: %s" % sid]
    if ts_list:
        lines.append("- Range: %s → %s"
                     % (ts_list[0][:19].replace("T", " "),
                        ts_list[-1][:19].replace("T", " ")))
    lines.append("")
    for e in events:
        role = e.get("role")
        text = e.get("text") or ""
        if role == "you":
            lines.append("**You:** %s" % text)
        elif role == "claude":
            lines.append("**%s:** %s" % (who, text))
        elif role == "tool":
            lines.append("`%s` %s" % (e.get("tool") or "tool", text))
        elif role == "system":
            lines.append("_system:_ %s" % text)
        lines.append("")
    return "\n".join(lines)


# --------------------------------------------------------------------------- #
# Arena cali proxy (California Burrito taco Tuesdays). The page talks only to
# these local routes; arena.cali_log_order() forwards an allowlisted body.
#
# The deal is buy-1-get-1 pooled across the whole table, so the page sends only
# who ordered what: TT, the paid count and TPP are all worked out by the server.
# --------------------------------------------------------------------------- #

# Mirrors backend/app/schemas.py (MAX_DINERS, MAX_PER_VARIANT, MAX_PER_ITEM,
# DINER_NAME_MAX); change both together. Validating here too means a typo gets a
# readable local error instead of a round trip to a 422.
CALI_MAX_DINERS = 20
CALI_MAX_PER_VARIANT = 50
CALI_MAX_PER_ITEM = 20
CALI_NAME_MAX = 40

_CALI_DATE_RE = re.compile(r"^\d{4}-\d{2}-\d{2}$")


def cali_body(body):
    """Validate a page order -> (clean body for arena.cali_log_order, None) or
    (None, error text). `clean` never contains a sessionId."""
    body = body if isinstance(body, dict) else {}
    rid = body.get("requestId")
    if not (isinstance(rid, str) and _RID_RE.fullmatch(rid)):
        return None, "invalid requestId"

    diners = body.get("diners")
    if not isinstance(diners, list) or not 1 <= len(diners) <= CALI_MAX_DINERS:
        return None, "an order needs 1 to %d diners" % CALI_MAX_DINERS

    clean_diners, seen = [], set()
    for d in diners:
        if not isinstance(d, dict):
            return None, "each diner must be an object"
        handle = d.get("handle")
        handle = handle.strip() if isinstance(handle, str) else ""
        if handle and not _HANDLE_RE.fullmatch(handle):
            return None, "invalid handle"
        name = d.get("name")
        name = " ".join("".join(c for c in name if c.isprintable()).split())[:CALI_NAME_MAX] \
            if isinstance(name, str) else ""
        if not handle and not name:
            return None, "every diner needs a handle or a name"
        # Same key the board groups on, so a double-entry is caught here.
        key = "@" + handle.lower() if handle else "#" + name.casefold()
        if key in seen:
            return None, "the same diner is listed twice"
        seen.add(key)

        tacos = d.get("tacos")
        tacos = tacos if isinstance(tacos, dict) else {}
        counts = {}
        for k in arena.CALI_TACO_KEYS:
            v = tacos.get(k, 0)
            if not _int_in(v, 0, CALI_MAX_PER_VARIANT):
                return None, ("each taco count must be a whole number from 0 to %d"
                              % CALI_MAX_PER_VARIANT)
            counts[k] = v

        row = {"tacos": counts}
        # The rest of the menu: known keys, whole numbers 0..CALI_MAX_PER_ITEM.
        # Zeros are dropped, and `items` is left off entirely when nothing is
        # left, because an Arena from before items 422s on the key itself.
        items = d.get("items")
        if items is not None and not isinstance(items, dict):
            return None, "items must be an object of menu item counts"
        kept = {}
        for k, v in (items or {}).items():
            if k not in arena.CALI_ITEM_KEYS:
                return None, "unknown menu item"
            if not _int_in(v, 0, CALI_MAX_PER_ITEM):
                return None, ("each menu item count must be a whole number from 0 to %d"
                              % CALI_MAX_PER_ITEM)
            if v > 0:
                kept[k] = v
        if kept:
            row["items"] = {k: kept[k] for k in arena.CALI_ITEM_KEYS if k in kept}
        if handle:
            row["handle"] = handle
        if name:
            row["name"] = name
        clean_diners.append(row)

    clean = {"requestId": rid, "diners": clean_diners}
    when = body.get("date")
    if when is not None:
        if not (isinstance(when, str) and _CALI_DATE_RE.fullmatch(when)):
            return None, "date must look like YYYY-MM-DD"
        clean["date"] = when
    note = body.get("note")
    clean["note"] = " ".join("".join(c for c in note if c.isprintable()).split())[:80] \
        if isinstance(note, str) else ""
    return clean, None


# --------------------------------------------------------------------------- #
# HTTP server
# --------------------------------------------------------------------------- #

# POST body ceilings. Sound uploads arrive base64-encoded in JSON, so their
# ceiling is MAX_SOUND_UPLOAD * 4/3 plus headroom; everything else is tiny.
MAX_POST_BODY = 1024 * 1024
MAX_SOUND_POST_BODY = 8 * 1024 * 1024


def _post_body_limit(path):
    return MAX_SOUND_POST_BODY if path == "/api/arena/sounds" else MAX_POST_BODY


# Every state-changing route. A path missing here 404s locally as "not found",
# which the page would misread as an Arena server without the feature.
# --------------------------------------------------------------------------- #
# Self-update: pull the latest commits (fast-forward only) and restart
# --------------------------------------------------------------------------- #
UPDATE_CHECK_SECS = 600
_update_cache = {"at": 0.0, "data": None}
_update_lock = threading.Lock()
# What this process is running, captured at start. When the code on disk moves on
# (a pull from a terminal, a local edit) the server keeps serving the old backend
# until it restarts; update_status reports that as "stale".
BOOT_ID = secrets.token_hex(8)
_CODE_FILES = (os.path.abspath(__file__),)


def _code_sig():
    try:
        return tuple(os.stat(f).st_mtime_ns for f in _CODE_FILES)
    except OSError:
        return ()


BOOT_CODE_SIG = _code_sig()


def _git(*args, timeout=30):
    env = dict(os.environ, GIT_TERMINAL_PROMPT="0")
    return subprocess.run(["git", "-C", HERE] + list(args), capture_output=True,
                          text=True, timeout=timeout, env=env)


def update_status(force=False):
    """How far this checkout is behind its upstream. Fetches at most every
    UPDATE_CHECK_SECS unless forced; never changes the working tree. Always
    says (uncached) whether this process is running the code on disk."""
    return dict(_update_status_git(force), stale=_code_sig() != BOOT_CODE_SIG)


def _update_status_git(force):
    with _update_lock:
        now = time.time()
        if not force and _update_cache["data"] and now - _update_cache["at"] < UPDATE_CHECK_SECS:
            return _update_cache["data"]
        if not os.path.isdir(os.path.join(HERE, ".git")) and not os.path.isfile(os.path.join(HERE, ".git")):
            return {"ok": False, "error": "this copy of Claude HQ is not a git checkout"}
        try:
            fetched = _git("fetch", "--quiet", "origin", timeout=60).returncode == 0
            up = _git("rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}")
            if up.returncode != 0:
                return {"ok": False, "error": "the current branch has no upstream to pull from"}
            counts = _git("rev-list", "--left-right", "--count", "HEAD...@{u}").stdout.split()
            ahead, behind = (int(counts[0]), int(counts[1])) if len(counts) == 2 else (0, 0)
            data = {
                "ok": True, "fetched": fetched,
                "branch": _git("rev-parse", "--abbrev-ref", "HEAD").stdout.strip(),
                "upstream": up.stdout.strip(),
                "head": _git("rev-parse", "--short", "HEAD").stdout.strip(),
                "ahead": ahead, "behind": behind,
                "dirty": bool(_git("status", "--porcelain", "--untracked-files=no").stdout.strip()),
                "commits": _git("log", "--format=%h %s", "-n", "10", "HEAD..@{u}").stdout.splitlines(),
                "version": APP_VERSION,
            }
        except (OSError, subprocess.SubprocessError) as e:
            return {"ok": False, "error": "git failed: %s" % e}
        _update_cache.update(at=now, data=data)
        return data


def _restart_self():
    """Replace this process with a fresh one. Under the launchd agent, ask
    launchd to restart it (KeepAlive brings it back); otherwise re-exec."""
    try:
        if os.getppid() == 1 and sys.platform == "darwin":
            subprocess.Popen(["launchctl", "kickstart", "-k", "gui/%d/%s" % (os.getuid(), LAUNCH_LABEL)],
                             start_new_session=True, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
            return
    except OSError:
        pass
    os.execv(sys.executable, [sys.executable] + sys.argv)


def update_and_restart():
    """Fast-forward to the upstream and restart. Refuses local edits and
    diverged history rather than merging or discarding anything."""
    st = update_status(force=True)
    if not st.get("ok"):
        if st.get("stale"):
            threading.Timer(1.0, _restart_self).start()
            return 200, {"ok": True, "updated": False, "restarting": True}
        return 409, st
    if not st["behind"] and st.get("stale"):
        # Nothing to pull, but the code on disk is newer than this process.
        _update_cache.update(at=0.0, data=None)
        threading.Timer(1.0, _restart_self).start()
        return 200, dict(st, updated=False, restarting=True)
    if st["dirty"]:
        return 409, dict(st, error="you have local changes in the Claude HQ folder; commit or stash them first")
    if st["ahead"] and st["behind"]:
        return 409, dict(st, error="your branch and %s have diverged; pull by hand" % st["upstream"])
    if not st["behind"]:
        return 200, dict(st, updated=False)
    old = st["head"]
    r = _git("merge", "--ff-only", "@{u}", timeout=60)
    if r.returncode != 0:
        return 409, dict(st, error="pull failed: %s" % (r.stderr.strip()[:300] or "unknown error"))
    new = _git("rev-parse", "--short", "HEAD").stdout.strip()
    _update_cache.update(at=0.0, data=None)
    threading.Timer(1.0, _restart_self).start()   # after this response is sent
    return 200, {"ok": True, "updated": True, "from": old, "to": new, "commits": st["commits"],
                 "count": st["behind"], "restarting": True}


# --------------------------------------------------------------------------- #
# Music: Now Playing + listen-along lookups (music.py does the work)
# --------------------------------------------------------------------------- #
def _music_share_on():
    return bool(load_config().get("musicShare")) and arena.status().get("paired")


# Room songs fetched on this machine with yt-dlp (git-ignored, ~400 MB at most).
MUSIC_AUDIO = music.AudioCache(os.path.join(HERE, ".music-cache"),
                               cookies=lambda: load_config().get("musicCookies"))

MUSIC_SHARE = music.ShareLoop(
    _music_share_on,
    put=lambda t: arena.music_now_put(t)[0],
    clear=lambda: arena.music_now_clear()[0],
)


def music_get(path, raw_path):
    """(code, body) for the Music GET routes."""
    import urllib.parse
    qs = urllib.parse.parse_qs(raw_path.split("?", 1)[1] if "?" in raw_path else "")
    arg = lambda k: (qs.get(k, [""])[0] or "").strip()
    try:
        if path == "/api/music/now":
            # This Mac's track (local only), plus whether it is being shared.
            # Fresh enough that the page's clock re-anchors on the player's own position.
            t, age = music.current(max_age=1.5, with_age=True)
            return 200, json.dumps({
                "track": t, "ageMs": int(age * 1000), "share": bool(load_config().get("musicShare")),
                "paired": bool(arena.status().get("paired")),
                "shared": MUSIC_SHARE.sent is not None,
                "shareError": MUSIC_SHARE.last_error,
                "platform": sys.platform,
            })
        if path == "/api/music/search":
            q = arg("q")
            vid = music.youtube_id_from_url(q)
            if vid:
                one = music.oembed(vid) or {"v": vid, "title": "", "author": ""}
                if one.get("embeddable") is False:
                    return 200, json.dumps({"results": [], "error": "That video's owner only lets it play on YouTube itself. Try another upload of the song."})
                return 200, json.dumps({"results": [one]})
            return 200, json.dumps({"results": music.search(q, verify=arg("quick") != "1")})
        if path == "/api/music/audio-state":
            # Starts fetching the song if it isn't here yet; "unavailable" = no yt-dlp.
            st = MUSIC_AUDIO.prepare(arg("v"))
            st["ffmpeg"] = music._tool("ffmpeg") is not None
            return 200, json.dumps(st)
        if path == "/api/music/oembed":
            one = music.oembed(arg("v"))
            return (200, json.dumps(one)) if one else (404, json.dumps({"error": "not found"}))
        code, resp = arena.music_now()
        return (code or 502), json.dumps(resp)
    except Exception as e:
        return 502, json.dumps({"error": "music request failed: %s" % e})

POST_PATHS = (
    "/api/update",
    "/api/action", "/api/config", "/api/meta", "/api/evo-choice",
    "/api/arena/pair", "/api/arena/unpair",
    "/api/arena/publish", "/api/arena/ticket",
    "/api/arena/nudge",
    "/api/arena/pantry/claim", "/api/arena/pantry/buy",
    "/api/arena/pantry/eat", "/api/arena/pantry/give",
    "/api/arena/pantry/reward",
    "/api/arena/hq/me",
    "/api/arena/portrait",
    "/api/arena/cosmetics/buy", "/api/arena/cosmetics/equip", "/api/arena/market/sell",
    "/api/arena/crews/create", "/api/arena/crews/join", "/api/arena/crews/leave",
    "/api/arena/quickplay/join", "/api/arena/quickplay/leave",
    "/api/arena/cali/order",
    "/api/arena/sounds",
    "/api/games/state",
) + ARENA_ROOM_POSTS + tuple(p for m in EXT for p in getattr(m, "POST", {}))


# --------------------------------------------------------------------------- #
# Valley minigames: static game scripts + one local save file
# --------------------------------------------------------------------------- #
# The games live in games/*.js|css so index.html does not grow; they are served
# from an allowlisted name pattern only (no subdirectories, no dotfiles), and
# their progress is a local JSON file that never leaves this machine.
GAMES_DIR = os.path.join(HERE, "games")
GAMES_SAVE_PATH = os.path.join(HERE, "games-save.json")
GAMES_SAVE_MAX = 256 * 1024
_GAME_FILE_RE = re.compile(r"[a-z][a-z0-9_-]{0,40}\.(js|css)")
# The vendored three.js modules and each 3D game's Kenney (CC0) models and data (Mini Golf,
# Kart Racing, Platformer Rush, Blaster Arena): exactly one folder level, lowercase names, these extensions only.
_GAME_ASSET_RE = re.compile(r"(vendor/[a-z][a-z0-9-]{0,40}\.js|(golf|kart|platformer|fps)/[a-z][a-z0-9-]{0,40}\.(glb|json|png))")
_GAME_TYPES = {"js": "application/javascript; charset=utf-8", "css": "text/css; charset=utf-8",
               "glb": "model/gltf-binary", "json": "application/json; charset=utf-8", "png": "image/png"}
_games_lock = threading.Lock()


class HQServer(ThreadingHTTPServer):
    """The stdlib default listen backlog is 5: a 3D game opening fetches a dozen models at
    once (more with two tabs open), and on macOS connections past the backlog are reset,
    which the browser reports as a failed model load. Same server otherwise."""
    request_queue_size = 128
    daemon_threads = True


# index.html is a template: its CSS and script live in ui/ (one file per area of the
# page) and are stitched back in, in order, where a whole line reads
# `//@include ui/...` or `/*@include ui/...*/`. The browser gets one page with the
# same single inline script it always had (same globals, hoisting and "use strict"),
# with no build step: edit a file in ui/ and reload.
_INCLUDE_RE = re.compile(r"^[ \t]*(?://@include (ui/[\w./-]+)|/\*@include (ui/[\w./-]+)\*/)[ \t]*$", re.M)
_index_memo = {"sig": None, "html": None}
_index_lock = threading.Lock()


def _include_path(rel):
    path = os.path.realpath(os.path.join(HERE, rel))
    root = os.path.realpath(UI_DIR)
    if ".." in rel.split("/") or not path.startswith(root + os.sep) or not os.path.isfile(path):
        raise FileNotFoundError(rel)
    return path


def assemble_index():
    """The full page: index.html with every @include line replaced by that file.
    Re-read whenever any part changes on disk (mtime), like index.html always was."""
    with open(INDEX_HTML, "r", encoding="utf-8") as f:
        tpl = f.read()
    rels = [m.group(1) or m.group(2) for m in _INCLUDE_RE.finditer(tpl)]
    paths = [_include_path(r) for r in rels]
    sig = (tpl,) + tuple(os.stat(p).st_mtime_ns for p in paths)
    with _index_lock:
        if _index_memo["sig"] == sig:
            return _index_memo["html"]
    parts = {}
    for r, p in zip(rels, paths):
        with open(p, "r", encoding="utf-8") as f:
            parts[r] = f.read()

    def put(m):
        body = parts[m.group(1) or m.group(2)]
        return body[:-1] if body.endswith("\n") else body   # the include line keeps its own newline

    html = _INCLUDE_RE.sub(put, tpl)
    with _index_lock:
        _index_memo.update(sig=sig, html=html)
    return html


def game_cache_control(name):
    """The big, rarely-changing Mini Golf files (three.js ~2 MB, models ~1.8 MB) are
    cacheable but always revalidated by ETag ("no-cache"), so a reload costs a 304 per
    file and an upgrade can never pair a stale module with a new one. The game scripts
    themselves stay no-store."""
    if isinstance(name, str) and _GAME_ASSET_RE.fullmatch(name):
        return "no-cache"
    return "no-store"


def game_etag(body):
    return '"' + hashlib.sha1(body).hexdigest()[:20] + '"'


def game_file(name):
    """(bytes, content type) for an allowlisted file under games/, else None."""
    if not isinstance(name, str):
        return None
    root = os.path.realpath(GAMES_DIR)
    if _GAME_FILE_RE.fullmatch(name):
        folder = root
    elif _GAME_ASSET_RE.fullmatch(name):
        folder = os.path.join(root, name.split("/", 1)[0])
    else:
        return None
    path = os.path.realpath(os.path.join(root, name))
    if os.path.dirname(path) != folder or not os.path.isfile(path):
        return None
    try:
        with open(path, "rb") as f:
            return f.read(), _GAME_TYPES[name.rsplit(".", 1)[1]]
    except OSError:
        return None


def load_games_save():
    try:
        with open(GAMES_SAVE_PATH, "r", encoding="utf-8") as f:
            data = json.load(f)
        return data if isinstance(data, dict) else {}
    except (OSError, ValueError):
        return {}


def save_games_save(body):
    """Replace the whole save. The page owns the shape; the server only checks
    it is a JSON object of bounded size, then writes it atomically (0600)."""
    state = body.get("state")
    if not isinstance(state, dict):
        raise ValueError("state must be an object")
    blob = json.dumps(state, separators=(",", ":"))
    if len(blob) > GAMES_SAVE_MAX:
        raise ValueError("save too large")
    with _games_lock:
        fd, tmp = tempfile.mkstemp(dir=HERE, prefix=".games-save.", suffix=".tmp")
        try:
            with os.fdopen(fd, "w", encoding="utf-8") as f:
                f.write(blob)
                f.flush()
                os.fsync(f.fileno())
            os.chmod(tmp, 0o600)
            os.replace(tmp, GAMES_SAVE_PATH)
        except BaseException:
            try:
                os.unlink(tmp)
            except OSError:
                pass
            raise
    return {"ok": True, "bytes": len(blob)}


class Handler(BaseHTTPRequestHandler):
    server_version = "ClaudeHQ/" + APP_VERSION

    def _host_ok(self):
        host = self.headers.get("Host", "")
        hostname = host.split(":")[0].strip().lower()
        return hostname in ("127.0.0.1", "localhost", "")

    def _send(self, code, body, content_type="application/json; charset=utf-8", cache="no-store", etag=None):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", cache)
        if etag:
            self.send_header("ETag", etag)
        self.end_headers()
        try:
            self.wfile.write(body)
        except Exception:
            pass

    def _send_audio(self, vid):
        fpath, ctype = MUSIC_AUDIO.file(vid)   # validates the id: no path from the URL reaches the disk
        if not fpath:
            self._send(404, json.dumps({"error": "not fetched"}))
            return
        size = os.path.getsize(fpath)
        start, end = 0, size - 1
        m = re.match(r"bytes=(\d*)-(\d*)$", (self.headers.get("Range") or "").strip())
        partial = bool(m and (m.group(1) or m.group(2)))
        if partial:
            if m.group(1):
                start = int(m.group(1))
                end = min(int(m.group(2)), size - 1) if m.group(2) else size - 1
            else:
                start = max(0, size - int(m.group(2)))
            if start > end or start >= size:
                self.send_response(416)
                self.send_header("Content-Range", "bytes */%d" % size)
                self.end_headers()
                return
        self.send_response(206 if partial else 200)
        self.send_header("Content-Type", ctype)
        self.send_header("Accept-Ranges", "bytes")
        self.send_header("Content-Length", str(end - start + 1))
        if partial:
            self.send_header("Content-Range", "bytes %d-%d/%d" % (start, end, size))
        self.send_header("Cache-Control", "no-store")
        self.end_headers()
        try:
            with open(fpath, "rb") as f:
                f.seek(start)
                left = end - start + 1
                while left > 0:
                    chunk = f.read(min(65536, left))
                    if not chunk:
                        break
                    self.wfile.write(chunk)
                    left -= len(chunk)
        except Exception:
            pass   # the page seeked or closed mid-file

    def _send_download(self, code, body, content_type, filename):
        if isinstance(body, str):
            body = body.encode("utf-8")
        self.send_response(code)
        self.send_header("Content-Type", content_type)
        self.send_header("Content-Length", str(len(body)))
        self.send_header("Cache-Control", "no-store")
        self.send_header("Content-Disposition",
                         'attachment; filename="%s"' % filename)
        self.end_headers()
        try:
            self.wfile.write(body)
        except Exception:
            pass

    def do_HEAD(self):
        """Header-only responses (so `curl -I` works for exports); never a body."""
        if not self._host_ok():
            self.send_response(403)
            self.send_header("Content-Type", "text/plain; charset=utf-8")
            self.end_headers()
            return
        path = self.path.split("?", 1)[0]
        self.send_response(200)
        self.send_header("Cache-Control", "no-store")
        if path == "/api/export.csv":
            self.send_header("Content-Type", "text/csv; charset=utf-8")
            self.send_header("Content-Disposition",
                             'attachment; filename="claude-hq-sessions.csv"')
        elif path == "/api/export.json":
            self.send_header("Content-Type", "application/json; charset=utf-8")
            self.send_header("Content-Disposition",
                             'attachment; filename="claude-hq-export.json"')
        elif path == "/":
            self.send_header("Content-Type", "text/html; charset=utf-8")
        else:
            self.send_header("Content-Type", "application/json; charset=utf-8")
        self.end_headers()

    def do_GET(self):
        if not self._host_ok():
            self._send(403, "Forbidden: local access only\n", "text/plain; charset=utf-8")
            return

        path = self.path.split("?", 1)[0]

        for _m in EXT:
            _fn = getattr(_m, "GET", {}).get(path)
            if _fn is not None:
                import urllib.parse
                qs = urllib.parse.parse_qs(self.path.split("?", 1)[1] if "?" in self.path else "")
                try:
                    code, resp = _fn(lambda k: (qs.get(k, [""])[0] or "").strip())
                except Exception as e:
                    code, resp = 502, {"error": "request failed: %s" % e}
                self._send(code or 502, json.dumps(resp))
                return

        if path == "/":
            try:
                html = assemble_index()
                # Inject the per-process CSRF token (same-origin can read it only).
                html = html.replace(CSRF_PLACEHOLDER, CSRF_TOKEN)
                self._send(200, html, "text/html; charset=utf-8")
            except FileNotFoundError:
                self._send(
                    200,
                    "<!doctype html><meta charset=utf-8><title>Claude Dashboard</title>"
                    "<body style='font-family:system-ui;padding:2rem'>"
                    "<h1>Claude Dashboard</h1><p>index.html not found next to dashboard.py. "
                    "API is live at <a href='/api/sessions'>/api/sessions</a>.</p>",
                    "text/html; charset=utf-8",
                )
            except Exception as e:
                self._send(500, f"error reading index.html: {e}\n", "text/plain; charset=utf-8")
            return

        if path == "/manifest.webmanifest":
            self._send(200, json.dumps({
                "name": "Claude HQ", "short_name": "Claude HQ",
                "start_url": "/", "scope": "/", "display": "standalone",
                "background_color": "#0c0d10", "theme_color": "#0c0d10",
                "description": "Local dashboard for your Claude Code sessions.",
                "icons": [{"src": "/icon.svg", "sizes": "any",
                           "type": "image/svg+xml", "purpose": "any maskable"}],
            }), "application/manifest+json; charset=utf-8")
            return

        if path == "/icon.svg":
            self._send(200,
                ICON_SVG,
                "image/svg+xml; charset=utf-8")
            return

        if path == "/sw.js":
            # network-first SW: never serves stale content, but enables install +
            # an offline fallback to the last cached shell.
            self._send(200,
                'const C="claude-hq-v7";'
                'self.addEventListener("install",e=>self.skipWaiting());'
                'self.addEventListener("activate",e=>e.waitUntil(self.clients.claim()));'
                'self.addEventListener("fetch",e=>{'
                'const u=new URL(e.request.url);'
                'if(e.request.method!=="GET"||u.pathname.startsWith("/api/"))return;'
                'e.respondWith(fetch(e.request).then(r=>{'
                'if(u.pathname==="/"){const c=r.clone();caches.open(C).then(x=>x.put("/",c));}'
                'return r;}).catch(()=>caches.match(u.pathname==="/"?"/":e.request)));'
                '});',
                "application/javascript; charset=utf-8")
            return

        if path.startswith("/games/"):
            name = path[len("/games/"):]
            got = game_file(name)
            if got is None:
                self._send(404, "not found\n", "text/plain; charset=utf-8")
            else:
                cache = game_cache_control(name)
                tag = game_etag(got[0]) if cache != "no-store" else None
                if tag and self.headers.get("If-None-Match", "") == tag:
                    self._send(304, b"", got[1], cache, tag)
                else:
                    self._send(200, got[0], got[1], cache, tag)
            return

        if path == "/api/games/state":
            self._send(200, json.dumps({"state": load_games_save()}))
            return

        if path == "/api/sessions":
            try:
                payload = build_payload_memo()
            except Exception as e:
                payload = {
                    "updated": now_utc().isoformat(),
                    "season": _empty_season(),
                    "sessions": [],
                    "error": f"internal error: {e}",
                }
            self._send(200, json.dumps(payload))
            return

        if path == "/api/arena/status":
            self._send(200, json.dumps(arena.status()))
            return

        if path == "/api/arena/sounds":
            # Local clips win (dev/testing); otherwise ask the Arena host.
            local = local_sounds()
            if local:
                self._send(200, json.dumps({"sounds": local, "source": "local"}))
                return
            try:
                code, resp = arena.list_sounds()
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(resp))
            return

        if path.startswith("/api/arena/sounds/"):
            import urllib.parse as _up
            fn = _up.unquote(path[len("/api/arena/sounds/"):])
            ext = os.path.splitext(fn)[1].lower()
            # One plain filename only -- no separators, no traversal, known type.
            if (not fn or "/" in fn or "\\" in fn or ".." in fn
                    or fn.startswith(".") or ext not in SOUND_TYPES):
                self._send(404, json.dumps({"error": "no such sound"}))
                return
            lp = os.path.join(LOCAL_SOUNDS_DIR, fn)
            if (os.path.isfile(lp) and os.path.dirname(os.path.realpath(lp))
                    == os.path.realpath(LOCAL_SOUNDS_DIR)):
                try:
                    with open(lp, "rb") as f:
                        data = f.read()
                except Exception as e:
                    self._send(500, json.dumps({"error": str(e)}))
                    return
                self._send(200, data, SOUND_TYPES[ext])
                return
            try:
                code, ctype, data = arena.get_sound(fn)
            except Exception as e:
                self._send(502, json.dumps({"error": "arena request failed: %s" % e}))
                return
            if code != 200 or not data:
                self._send(code or 502, json.dumps({"error": "no such sound"}))
                return
            self._send(200, data, ctype or SOUND_TYPES[ext])
            return

        if path == "/api/arena/preview":
            code, resp = arena.preview(PROJECTS_DIR)
            self._send(code or 200, json.dumps(resp))
            return

        if path == "/api/arena/rooms":
            code, resp = arena.rooms_directory()
            self._send(code or 502, json.dumps(resp))
            return

        if path == "/api/arena/rooms/members":
            import urllib.parse as _up
            qs = _up.parse_qs(self.path.split("?", 1)[1]
                              if "?" in self.path else "")
            rid = (qs.get("roomId", [""])[0] or "")
            if not arena.ROOM_ID_RE.fullmatch(rid):
                self._send(400, json.dumps({"error": "roomId required"}))
                return
            code, resp = arena.room_members(rid)
            self._send(code or 502, json.dumps(resp))
            return

        if path == "/api/arena/board":
            import urllib.parse
            qs = urllib.parse.parse_qs(self.path.split("?", 1)[1]
                                       if "?" in self.path else "")
            window = (qs.get("window", ["season"])[0] or "season")
            if window not in ("season", "30d", "7d", "all"):
                window = "season"
            code, resp = arena.board(window)
            self._send(code or 502, json.dumps(resp))
            return

        if path == "/api/arena/pantry":
            # Read-only here and on the server: GETs never claim or drain.
            try:
                code, resp = arena.pantry()
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(
                _overlay_food_effects(resp) if code == 200 else resp))
            return

        if path == "/api/arena/quickplay/status":
            try:
                code, resp = arena.quickplay("status")
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(resp))
            return

        if path in ("/api/arena/crews", "/api/arena/crews/mine"):
            try:
                code, resp = arena.crews("mine" if path.endswith("/mine") else "board")
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(resp))
            return

        if path == "/api/arena/cosmetics":
            try:
                code, resp = arena.cosmetics()
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(resp))
            return

        if path == "/api/server-stats":
            self._send(200, json.dumps(server_stats()))
            return

        if path == "/api/arena/server-stats":
            try:
                code, resp = arena.server_stats()
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(resp))
            return

        if path in ("/api/arena/progress", "/api/arena/leaderboards", "/api/arena/profile"):
            # HQ 2.1: your level (sessions + games), per-game boards, a trainer card. Read-only.
            import urllib.parse
            qs = urllib.parse.parse_qs(self.path.split("?", 1)[1] if "?" in self.path else "")
            arg = lambda k: (qs.get(k, [""])[0] or "").strip()
            try:
                if path == "/api/arena/progress":
                    code, resp = arena.progress()
                elif path == "/api/arena/leaderboards":
                    code, resp = arena.leaderboards(arg("game"), arg("key") or None)
                else:
                    code, resp = arena.profile(arg("u") or "me")
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(resp))
            return

        if path.startswith("/api/music/audio/"):
            # A fetched room song, with Range so the page can seek in it.
            self._send_audio(path[len("/api/music/audio/"):])
            return

        if path in ("/api/music/now", "/api/music/search", "/api/music/oembed",
                    "/api/music/audio-state", "/api/arena/music/now"):
            self._send(*music_get(path, self.path))
            return

        if path in ("/api/arena/hq/me", "/api/arena/hq/open", "/api/arena/hq/visit"):
            # HQ 2.1: your building's look/openness, the open HQs, one HQ to visit.
            try:
                if path == "/api/arena/hq/me":
                    code, resp = arena.hq_me()
                elif path == "/api/arena/hq/open":
                    code, resp = arena.hq_open()
                else:
                    import urllib.parse
                    qs = urllib.parse.parse_qs(self.path.split("?", 1)[1] if "?" in self.path else "")
                    code, resp = arena.hq_visit((qs.get("u", [""])[0] or "").strip())
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(resp))
            return

        if path == "/api/arena/cali/board":
            import urllib.parse
            qs = urllib.parse.parse_qs(self.path.split("?", 1)[1]
                                       if "?" in self.path else "")
            window = (qs.get("window", ["season"])[0] or "season")
            if window not in arena.CALI_WINDOWS:
                window = "season"
            try:
                code, resp = arena.cali_board(window)
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(resp))
            return

        if path == "/api/arena/cali/orders":
            try:
                code, resp = arena.cali_orders()
            except Exception as e:
                code, resp = 502, {"error": "arena request failed: %s" % e}
            self._send(code or 502, json.dumps(resp))
            return

        if path == "/api/update":
            force = "force=1" in (self.path.split("?", 1)[1] if "?" in self.path else "")
            self._send(200, json.dumps(update_status(force=force)))
            return

        if path == "/api/config":
            try:
                self._send(200, json.dumps(load_config()))
            except Exception:
                self._send(200, json.dumps(dict(DEFAULT_CONFIG)))
            return

        if path == "/api/meta":
            try:
                self._send(200, json.dumps(load_meta()))
            except Exception:
                self._send(200, json.dumps({}))
            return

        if path.startswith("/api/transcript/"):
            sid = path[len("/api/transcript/"):]
            if not _UUID_RE.fullmatch(sid or ""):
                self._send(404, json.dumps({"error": "unknown session"}))
                return
            tpath = find_transcript(sid)
            if not tpath:
                self._send(404, json.dumps({"error": "unknown session"}))
                return
            try:
                import urllib.parse
                qs = urllib.parse.parse_qs(self.path.split("?", 1)[1]
                                           if "?" in self.path else "")
                try:
                    offset = int(qs.get("offset", ["0"])[0])
                except Exception:
                    offset = 0
                try:
                    limit = int(qs.get("limit", ["60"])[0])
                except Exception:
                    limit = 60
                if offset < 0:
                    offset = 0
                limit = max(1, min(200, limit))
                q = (qs.get("q", [""])[0] or "").strip().lower()
                events = get_transcript_events(tpath)
                agg = scan_file(tpath)
                title = session_heading(agg if isinstance(agg, dict) else {}, sid)
                matched = None
                if q:
                    events = [e for e in events
                              if q in (e.get("text") or "").lower()
                              or q in (e.get("tool") or "").lower()]
                    matched = len(events)
                page = events[offset:offset + limit]
                self._send(200, json.dumps({
                    "sessionId": sid, "title": title, "total": len(events),
                    "matched": matched, "query": q,
                    "offset": offset, "limit": limit, "events": page,
                    "source": (agg.get("source") if isinstance(agg, dict) else None)
                              or "claude",
                }))
            except Exception as e:
                self._send(404, json.dumps({"error": "unknown session: %s" % e}))
            return

        if path.startswith("/api/session/") and path.endswith("/export.md"):
            sid = path[len("/api/session/"):-len("/export.md")]
            if not _UUID_RE.fullmatch(sid or ""):
                self._send(404, json.dumps({"error": "unknown session"}))
                return
            tpath = find_transcript(sid)
            if not tpath:
                self._send(404, json.dumps({"error": "unknown session"}))
                return
            try:
                md = build_session_markdown(sid, tpath)
            except Exception as e:
                self._send(404, json.dumps({"error": "unknown session: %s" % e}))
                return
            self._send_download(200, md, "text/markdown; charset=utf-8",
                                "claude-hq-session-%s.md"
                                % ((sid or "")[:8] or "session"))
            return

        if path.startswith("/api/session/"):
            sid = path[len("/api/session/"):]
            # only a UUID-shaped session id is ever looked up
            if not _UUID_RE.fullmatch(sid or ""):
                self._send(404, json.dumps({"error": "unknown session"}))
                return
            try:
                detail = build_session_detail(sid)
            except Exception as e:
                self._send(404, json.dumps({"error": f"unknown session: {e}"}))
                return
            if detail is None:
                self._send(404, json.dumps({"error": "unknown session"}))
                return
            self._send(200, json.dumps(detail))
            return

        if path == "/api/stream":
            try:
                self.send_response(200)
                self.send_header("Content-Type", "text/event-stream; charset=utf-8")
                self.send_header("Cache-Control", "no-store")
                self.send_header("Connection", "keep-alive")
                self.end_headers()
            except Exception:
                return
            last_sig, last_sent = None, 0.0
            try:
                while True:
                    try:
                        blob, sig = build_payload_blob()
                    except Exception as e:
                        blob = json.dumps({"error": str(e)})
                        sig = blob
                    now_m = time.monotonic()
                    # A frame when something the page shows changed, else one per
                    # heartbeat so ages move on; keepalives in between.
                    if sig != last_sig or now_m - last_sent >= STREAM_HEARTBEAT_SECS:
                        self.wfile.write(("data: " + blob + "\n\n").encode("utf-8"))
                        last_sig, last_sent = sig, now_m
                    else:
                        self.wfile.write(b":keepalive\n\n")
                    self.wfile.flush()
                    time.sleep(1.5)
            except (BrokenPipeError, ConnectionResetError):
                return
            except Exception:
                return
            return

        if path == "/api/search":
            try:
                import urllib.parse
                qs = urllib.parse.parse_qs(self.path.split("?", 1)[1]
                                           if "?" in self.path else "")
                q = (qs.get("q", [""])[0] or "").strip()
                if not q:
                    self._send(200, json.dumps({"query": qs.get("q", [""])[0],
                                                "count": 0, "results": []}))
                    return
                results = search_transcripts(q)
                self._send(200, json.dumps({
                    "query": q, "count": len(results), "results": results,
                }))
            except Exception as e:
                self._send(200, json.dumps({"query": "", "count": 0,
                                            "results": [], "error": str(e)}))
            return

        if path == "/api/project":
            try:
                import urllib.parse
                qs = urllib.parse.parse_qs(self.path.split("?", 1)[1]
                                           if "?" in self.path else "")
                slug = (qs.get("folder", [""])[0] or "").strip()
                data = compute_project(slug)
                if data is None:
                    self._send(404, json.dumps({"error": "unknown folder"}))
                    return
                self._send(200, json.dumps(data))
            except Exception as e:
                self._send(200, json.dumps({
                    "folder": "", "prettyFolder": "", "totals": {},
                    "heatmap": [], "topFiles": [], "models": [], "sessions": [],
                    "error": str(e),
                }))
            return

        if path == "/api/history":
            try:
                self._send(200, json.dumps(compute_history()))
            except Exception as e:
                self._send(200, json.dumps({
                    "heatmap": [], "daily": [], "byHour": [0] * 24,
                    "byDow": [0] * 7, "totals": {}, "hallOfFame": [],
                    "error": str(e),
                }))
            return

        if path == "/api/insights":
            try:
                self._send(200, json.dumps(compute_insights()))
            except Exception as e:
                self._send(200, json.dumps({
                    "generated": now_utc().isoformat(),
                    "insights": [], "error": str(e),
                }))
            return

        if path == "/api/pokedex":
            try:
                self._send(200, json.dumps(compute_pokedex()))
            except Exception as e:
                self._send(200, json.dumps({
                    "caughtCount": 0, "total": 48, "shinyCount": 0,
                    "species": [], "error": str(e),
                }))
            return

        if path == "/api/export.json":
            try:
                p = build_payload_memo()
                body = json.dumps({
                    "generated": now_utc().isoformat(),
                    "season": p.get("season"),
                    "sessions": _full_text_sessions(p.get("sessions", [])),
                })
            except Exception as e:
                body = json.dumps({"generated": now_utc().isoformat(),
                                   "error": str(e)})
            self._send_download(200, body, "application/json; charset=utf-8",
                                "claude-hq-export.json")
            return

        if path == "/api/export.csv":
            try:
                body = build_export_csv(build_payload_memo())
            except Exception as e:
                body = "error\n%s\n" % str(e).replace("\n", " ")
            self._send_download(200, body, "text/csv; charset=utf-8",
                                "claude-hq-sessions.csv")
            return

        if path == "/api/digest":
            try:
                import urllib.parse
                qs = urllib.parse.parse_qs(self.path.split("?", 1)[1]
                                           if "?" in self.path else "")
                diso = (qs.get("date", [""])[0] or "").strip()
                if not re.match(r"^\d{4}-\d{2}-\d{2}$", diso):
                    diso = now_utc().astimezone().date().isoformat()
                try:
                    days = int(qs.get("days", ["1"])[0])
                except Exception:
                    days = 1
                days = max(1, min(31, days))
                data = compute_digest(diso, days)
                if qs.get("download", ["0"])[0] in ("1", "true", "yes"):
                    self._send_download(200, data["markdown"],
                                        "text/markdown; charset=utf-8",
                                        "claude-hq-digest-%s.md" % diso)
                    return
                self._send(200, json.dumps(data))
            except Exception as e:
                self._send(200, json.dumps({
                    "date": "", "markdown": "# Digest error\n\n%s" % e,
                    "sessionCount": 0, "totals": {}, "error": str(e),
                }))
            return

        self._send(404, json.dumps({"error": "not found"}))

    # ----- state-changing actions (guarded) ------------------------------- #

    def _origin_ok(self):
        """Reject cross-site POSTs even if they carry a valid Host header."""
        origin = self.headers.get("Origin")
        if origin:
            try:
                import urllib.parse
                host = urllib.parse.urlparse(origin).hostname
            except Exception:
                return False
            if host not in ("127.0.0.1", "localhost"):
                return False
        sfs = (self.headers.get("Sec-Fetch-Site") or "").strip().lower()
        if sfs == "cross-site":
            return False
        return True

    def _arena_post(self, path, body):
        """Arena actions. The device token never crosses back to the page."""
        for _m in EXT:
            _fn = getattr(_m, "POST", {}).get(path)
            if _fn is not None:
                try:
                    code, resp = _fn(body)
                    return (code or 502), resp
                except Exception as e:
                    return 502, {"error": "arena request failed: %s" % e}
        try:
            if path in ARENA_ROOM_POSTS:
                return _room_post(path, body)
            if path == "/api/arena/pair":
                code = body.get("code")
                if not isinstance(code, str) or not code.strip():
                    return 400, {"error": "pairing code required"}
                return arena.pair(code, label=body.get("label", ""))
            if path == "/api/arena/unpair":
                arena.clear_link()
                return 200, {"ok": True}
            if path == "/api/arena/publish":
                return arena.publish(PROJECTS_DIR)
            if path == "/api/arena/ticket":
                return arena.ws_ticket()
            if path == "/api/arena/nudge":
                to = body.get("toHandle")
                if not isinstance(to, str) or not to.strip():
                    return 400, {"error": "toHandle required"}
                return arena.send_nudge(to.strip(), note=body.get("note", ""))
            if path in ("/api/arena/quickplay/join", "/api/arena/quickplay/leave"):
                return arena.quickplay(path.rsplit("/", 1)[1], body)
            if path in ("/api/arena/crews/create", "/api/arena/crews/join", "/api/arena/crews/leave"):
                return arena.crews(path.rsplit("/", 1)[1], body)
            if path in ("/api/arena/cosmetics/buy", "/api/arena/cosmetics/equip"):
                return arena.cosmetics(path.rsplit("/", 1)[1], body)
            if path == "/api/arena/market/sell":
                return arena.market_sell(body.get("cat"), body.get("qty"))
            if path == "/api/arena/portrait":
                # Your 3D character portrait for everyone's lists (a PNG your page
                # rendered from your own choices), or {"remove": true}.
                if body.get("remove") is True:
                    return arena.portrait_delete()
                png = arena.portrait_png(body.get("png"))
                if png is None:
                    return 400, {"error": "png must be a data:image/png;base64 URL of a small square PNG"}
                return arena.portrait_put(png)
            if path == "/api/arena/hq/me":
                # Crew counts come from this process's own view of your sessions,
                # never from the page: only three numbers can leave.
                crew = hq_crew_counts() if body.get("crew") else None
                return arena.hq_update(open_=body.get("open"), look=body.get("look"), crew=crew)
            if path.startswith("/api/arena/pantry/"):
                action = path[len("/api/arena/pantry/"):]
                if action == "reward":
                    return _quest_reward(body)
                if action == "eat":
                    return pantry_eat(body)
                clean, err = pantry_body(action, body)
                if err:
                    return 400, {"error": err}
                code, resp = arena.pantry(action, clean)
                return (code or 502), (_overlay_food_effects(resp) if code == 200 else resp)
            if path == "/api/arena/cali/order":
                clean, err = cali_body(body)
                if err:
                    return 400, {"error": err}
                code, resp = arena.cali_log_order(clean)
                return (code or 502), resp
            if path == "/api/arena/sounds":
                return receive_sound_upload(body)
        except Exception as e:
            return 500, {"error": "arena request failed: %s" % e}
        return 404, {"error": "not found"}

    def do_POST(self):
        if not self._host_ok():
            self._send(403, json.dumps({"error": "local access only"}))
            return
        # CSRF: exact token match required (same-origin page only can supply it).
        token = self.headers.get("X-HQ-Token", "")
        if not token or not secrets.compare_digest(token, CSRF_TOKEN):
            self._send(403, json.dumps({"error": "bad or missing CSRF token"}))
            return
        if not self._origin_ok():
            self._send(403, json.dumps({"error": "cross-site request rejected"}))
            return

        path = self.path.split("?", 1)[0]
        if path not in POST_PATHS:
            self._send(404, json.dumps({"error": "not found"}))
            return

        cl = (self.headers.get("Content-Length") or "").strip()
        if not (cl.isascii() and cl.isdigit()):
            # Missing (e.g. chunked) or garbage length: we never read an
            # unbounded body. The connection can't be reused safely either.
            self.close_connection = True
            self._send(411 if not cl else 400,
                       json.dumps({"error": "a valid Content-Length is required"}))
            return
        length = int(cl)
        if length > _post_body_limit(path):
            # Reply without reading the body (a DoS guard). A browser still
            # uploading may see a connection reset instead of this 413; the
            # UI pre-checks sizes (e.g. the 5 MB sound cap), so that's rare.
            self.close_connection = True
            self._send(413, json.dumps({"error": "request body too large"}))
            return

        try:
            raw = self.rfile.read(length) if length > 0 else b""
            body = json.loads(raw.decode("utf-8")) if raw else {}
            if not isinstance(body, dict):
                raise ValueError("body must be an object")
        except Exception as e:
            self._send(400, json.dumps({"error": "bad JSON body: %s" % e}))
            return

        if path.startswith("/api/arena/"):
            code, resp = self._arena_post(path, body)
            self._send(code, json.dumps(resp))
            return

        if path == "/api/update":
            code, resp = update_and_restart()
            self._send(code, json.dumps(resp))
            return

        if path == "/api/config":
            try:
                self._send(200, json.dumps(save_config(body)))
            except OSError as e:
                self._send(500, json.dumps({"error": "could not save config: %s" % e}))
            except Exception as e:
                self._send(400, json.dumps({"error": "bad config: %s" % e}))
            return

        if path == "/api/games/state":
            try:
                self._send(200, json.dumps(save_games_save(body)))
            except ValueError as e:
                self._send(400, json.dumps({"error": "bad save: %s" % e}))
            except OSError as e:
                self._send(500, json.dumps({"error": "could not write save: %s" % e}))
            return

        if path == "/api/evo-choice":
            sid = body.get("sessionId")
            try:
                entry = save_evo_choice(sid, body)
            except ValueError as e:
                self._send(400, json.dumps({"error": str(e)}))
                return
            except OSError as e:
                self._send(500, json.dumps({"error": "could not save the pick: %s" % e}))
                return
            _invalidate_payload_memo()            # the next /api/sessions carries it
            self._send(200, json.dumps({"sessionId": sid, "evo": entry}))
            return

        if path == "/api/meta":
            sid = body.get("sessionId")
            if not isinstance(sid, str) or not _UUID_RE.match(sid):
                self._send(400, json.dumps({"error": "invalid sessionId"}))
                return
            try:
                self._send(200, json.dumps(save_meta(sid, body)))
            except OSError as e:
                self._send(500, json.dumps({"error": "could not save meta: %s" % e}))
            except Exception as e:
                self._send(400, json.dumps({"error": "bad meta: %s" % e}))
            return

        action = body.get("action")
        try:
            if action == "resume":
                code, resp = action_resume(body.get("sessionId"))
            elif action == "reveal":
                code, resp = action_reveal(body.get("sessionId"))
            elif action == "close":
                code, resp = action_close(body.get("pid"))
            else:
                code, resp = 400, {"error": "unknown action"}
        except Exception as e:
            code, resp = 500, {"error": "action failed: %s" % e}
        self._send(code, json.dumps(resp))

    def log_message(self, fmt, *args):
        # keep the console quiet-ish
        return


def _applescript_str(s):
    """An AppleScript string literal. json.dumps escapes " and \\, but its default
    \\uXXXX escapes (every emoji) are an AppleScript syntax error, so non-ASCII
    stays as-is and control characters are dropped."""
    return json.dumps("".join(c for c in str(s) if c.isprintable()), ensure_ascii=False)


def _notify(title, body, sound=True):
    """Native macOS notification: nudges ping, gifts arrive silently."""
    try:
        subprocess.run(
            ["osascript", "-e",
             "display notification %s with title %s%s"
             % (_applescript_str(body), _applescript_str(title),
                ' sound name "Ping"' if sound else "")],
            check=False, timeout=10,
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    except Exception:
        pass


def main():
    ap = argparse.ArgumentParser(description="Local Claude sessions dashboard.")
    ap.add_argument("--port", type=int, default=8765)
    ap.add_argument("--no-open", action="store_true")
    ap.add_argument("--print-plist", action="store_true",
                    help="Print the launchd LaunchAgent plist and exit (no side effects).")
    ap.add_argument("--install", action="store_true",
                    help="Install a launchd LaunchAgent so Claude HQ starts at login.")
    ap.add_argument("--uninstall", action="store_true",
                    help="Remove the launchd LaunchAgent.")
    args = ap.parse_args()

    global SERVER_PORT
    SERVER_PORT = args.port

    if args.print_plist:
        sys.stdout.write(render_plist(args.port))
        return
    if args.uninstall:
        uninstall_launchagent()
        return
    if args.install:
        install_launchagent(args.port)
        return

    url = f"http://127.0.0.1:{args.port}"
    server = HQServer(("127.0.0.1", args.port), Handler)

    # Arena (multiplayer) stays dormant until the user pairs and enables it.
    arena.init(scan_file, load_config, HERE)
    arena.start_publisher(PROJECTS_DIR)
    for _m in EXT:
        if hasattr(_m, "start"):
            try:
                _m.start({"arena": arena, "load_config": load_config, "scan_file": scan_file,
                          "iter_transcript_paths": iter_transcript_paths, "base_dir": HERE})
            except Exception:
                pass
    # Now Playing: share the track this Mac plays while paired and musicShare is on.
    MUSIC_SHARE.start()

    # Raise a native macOS notification for an incoming nudge or gift, so it
    # reaches you even with no Arena tab open (as long as this process is running).
    arena.start_nudge_poller(_notify)

    # Warm the per-file scan + search caches in the background so the first
    # /api/search and /api/history are instant instead of a one-time ~1s scan.
    def _prewarm():
        try:
            for p in iter_transcript_paths():
                try:
                    scan_file(p)
                    get_search_entry(p)
                except Exception:
                    continue
        except Exception:
            pass
    threading.Thread(target=_prewarm, daemon=True).start()

    if not args.no_open:
        try:
            webbrowser.open(url)
        except Exception:
            pass

    print(f"Claude Dashboard serving at {url}  (127.0.0.1 only — private)")
    print("Press Ctrl+C to stop.")
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        print("\nShutting down.")
        server.shutdown()


if __name__ == "__main__":
    main()
