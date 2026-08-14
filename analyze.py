#!/usr/bin/env python3
"""DiskAnalyzer entry point.

    python analyze.py                 scan C: without the live Windows install
    python analyze.py --duplicates    also look for duplicate files
    python analyze.py --help          every option

This tool only reads. It has no code path that deletes, moves or modifies a
file, and it verifies that about itself before every scan.
"""

import sys

from diskanalyzer.cli import main

if __name__ == "__main__":
    sys.exit(main())
