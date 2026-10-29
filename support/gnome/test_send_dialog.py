"""Only the legacy launch bridge remains in Python; GTK tests live in picker/."""
import importlib.util
from pathlib import Path
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('send_dialog', Path(__file__).parent / 'localsend-transfer@local/send_dialog.py')
dialog = importlib.util.module_from_spec(spec)
spec.loader.exec_module(dialog)


class LauncherTests(unittest.TestCase):
    def test_replaces_python_with_sibling_rust_binary_without_shell_parsing(self):
        binary = '/tmp/中文 folder/$(touch no)/localsend-cli'
        picker = '/tmp/中文 folder/$(touch no)/localsend-picker'
        with patch('sys.argv', ['send_dialog.py', '--binary', binary]), patch.object(dialog.os, 'execv') as execute:
            dialog.main()
        execute.assert_called_once_with(picker, [picker])


if __name__ == '__main__':
    unittest.main()
