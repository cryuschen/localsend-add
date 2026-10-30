# Modified by localsend-add contributors, 2026. See NOTICE and LICENSE.
"""Restore only panel-install.json's owned files; preserve current user settings."""
import ast
import hashlib
import json
from pathlib import Path
import subprocess
import os
import tempfile


def replace(source,target,mode):
    fd,name=tempfile.mkstemp(prefix='.rollback-panel-',dir=target.parent)
    try:
        with os.fdopen(fd,'wb') as stream:
            stream.write(source.read_bytes());os.fchmod(stream.fileno(),mode)
        os.replace(name,target)
    finally:
        if os.path.exists(name):os.unlink(name)


root=Path(__file__).resolve().parents[3]
manifest=json.loads((root/'development-setup/panel-install.json').read_text())
cli=Path.home()/'.local/bin/localsend-cli'
restart_backend=manifest.get('restart_backend',manifest['was_running'])
extension_dir=Path.home()/'.local/share/gnome-shell/extensions/localsend-transfer@local'
updates_extension=any(Path(record['path']).parent==extension_dir for record in manifest['files'])
if restart_backend:subprocess.run([cli,'stop'],check=True,capture_output=True,text=True)
if updates_extension:subprocess.run(['gnome-extensions','disable','localsend-transfer@local'],check=False,capture_output=True)
for record in manifest['files']:
    path=Path(record['path'])
    actual=hashlib.sha256(path.read_bytes()).hexdigest() if path.exists() else None
    if actual!=record['installed_sha256']:
        raise SystemExit('File has changed; inspect before rollback: '+str(path))
for record in manifest['files']:
    path=Path(record['path'])
    if record['original_sha256'] is None:
        path.unlink(missing_ok=True)
    else:
        saved=Path(record['backup'])
        assert hashlib.sha256(saved.read_bytes()).hexdigest()==record['original_sha256']
        replace(saved,path,saved.stat().st_mode&0o777)
# Restore only this extension's previous membership, preserving other changes.
for key, before in ([('enabled-extensions','enabled_before'),('disabled-extensions','disabled_before')] if updates_extension else []):
    values=ast.literal_eval(subprocess.check_output(['gsettings','get','org.gnome.shell',key],text=True))
    values=[value for value in values if value!='localsend-transfer@local']
    if 'localsend-transfer@local' in manifest[before]:values.append('localsend-transfer@local')
    subprocess.run(['gsettings','set','org.gnome.shell',key,repr(values)],check=True)
if restart_backend:subprocess.run([cli,'start'],check=True)
print('Previous files restored. Configuration preserved.' + (' Log out/in to reload the extension.' if updates_extension else ' No relogin needed.'))
