"""Exercise picker-only installation and rollback in a temporary fake home."""
import json
from pathlib import Path
import runpy
import shutil
import subprocess
import tempfile
import unittest
from unittest.mock import patch


class PickerInstallationTests(unittest.TestCase):
    def test_install_and_rollback_preserve_running_backend_menu_and_configuration(self):
        source = Path(__file__).parent
        with tempfile.TemporaryDirectory(prefix='localsend-picker-install-') as temporary:
            root = Path(temporary)
            support = root / 'source/support/gnome'
            support.mkdir(parents=True)
            (root / 'development-setup').mkdir()
            for name in ['install_panel.py', 'rollback_panel.py']:
                shutil.copy2(source / name, support / name)
            extension_source = support / 'localsend-transfer@local'
            extension_source.mkdir()
            for name in ['extension.js', 'metadata.json', 'send_dialog.py']:
                shutil.copy2(source / 'localsend-transfer@local' / name, extension_source / name)
            binary_source = root / 'source/target/release/localsend-picker'
            binary_source.parent.mkdir(parents=True)
            binary_source.write_bytes(b'rust-picker-fixture')
            home = root / 'home'
            cli = home / '.local/bin/localsend-cli'
            menu = home / '.local/share/nautilus-python/extensions/localsend_menu.py'
            extension = home / '.local/share/gnome-shell/extensions/localsend-transfer@local'
            config = home / '.config/localsend-cli'
            unchanged = [cli, menu, *(config / name for name in ['config.toml', 'identity.pem', 'paired-v2.json'])]
            for path in unchanged:
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'preserve:' + path.name.encode())
            originals = {path: path.read_bytes() for path in unchanged}
            extension.mkdir(parents=True)
            for name in ['extension.js', 'metadata.json', 'send_dialog.py']:
                (extension / name).write_text('previous:' + name)
            (extension / 'stylesheet.css').write_text('preserve ring styling')

            def output(command, **kwargs):
                command = [str(part) for part in command]
                if command == [str(cli), '--version']:
                    return 'localsend-cli 1.18.2-local-unified.4\n'
                if command == [str(cli), 'status', '--json']:
                    # Even an active transfer must not be restarted by picker-only.
                    return json.dumps({'running': True, 'sending': True})
                if command == [str(binary_source), '--version']:
                    return 'localsend-picker 1.18.2\n'
                if command[:2] == ['gsettings', 'get']:
                    return "['localsend-transfer@local', 'other-extension']" if command[-1] == 'enabled-extensions' else '[]'
                self.fail('Unexpected process: ' + repr(command))

            with patch.dict('os.environ', {'HOME': str(home)}), \
                    patch('sys.argv', ['install_panel.py', '--picker-only']), \
                    patch('subprocess.check_output', side_effect=output), \
                    patch('subprocess.run', return_value=subprocess.CompletedProcess([], 0)) as execute:
                runpy.run_path(str(support / 'install_panel.py'), run_name='__main__')
                manifest = json.loads((root / 'development-setup/panel-install.json').read_text())
                self.assertFalse(manifest['restart_backend'])
                self.assertFalse(manifest['requires_files_reload'])
                self.assertTrue(manifest['picker_available_now'])
                self.assertEqual(len(manifest['files']), 4)
                self.assertEqual((home / '.local/bin/localsend-picker').read_bytes(), binary_source.read_bytes())
                self.assertEqual((home / '.local/bin/localsend-picker').stat().st_mode & 0o777, 0o755)
                self.assertEqual((extension / 'stylesheet.css').read_text(), 'preserve ring styling')
                self.assertEqual({path: path.read_bytes() for path in unchanged}, originals)
                execute.assert_not_called()
                runpy.run_path(str(support / 'rollback_panel.py'), run_name='__main__')
                self.assertFalse((home / '.local/bin/localsend-picker').exists())
                for name in ['extension.js', 'metadata.json', 'send_dialog.py']:
                    self.assertEqual((extension / name).read_text(), 'previous:' + name)
                self.assertEqual({path: path.read_bytes() for path in unchanged}, originals)
                self.assertFalse(any(str(cli) == str(call.args[0][0]) for call in execute.call_args_list))


if __name__ == '__main__':
    unittest.main()
