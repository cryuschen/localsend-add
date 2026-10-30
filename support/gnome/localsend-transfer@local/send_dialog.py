# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Compatibility entry for Shell sessions loaded before the Rust picker update.

Replaced in-place with the Rust process; no Python GTK UI or menu import remains.
New Shell sessions launch localsend-picker directly.
"""
import argparse
import os
from pathlib import Path


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', required=True)
    args = parser.parse_args()
    picker = str(Path(args.binary).with_name('localsend-picker'))
    os.execv(picker, [picker])


if __name__ == '__main__':
    main()
