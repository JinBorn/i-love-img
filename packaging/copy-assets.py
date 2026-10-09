#!/usr/bin/env python3
"""Copy runtime assets while keeping source-only full fonts out of packages."""
import argparse
import pathlib
import shutil

parser = argparse.ArgumentParser()
parser.add_argument("source", type=pathlib.Path)
parser.add_argument("destination", type=pathlib.Path)
args = parser.parse_args()
shutil.copytree(args.source, args.destination,
                ignore=shutil.ignore_patterns("*.full.otf", "*.full.ttf", "__pycache__"))
print("Copied runtime assets; source-only full font files excluded.")
