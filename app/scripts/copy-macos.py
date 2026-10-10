import ctypes
import json
import os
import stat
import sys

library = ctypes.CDLL('/usr/lib/libSystem.B.dylib', use_errno=True)
clonefile = library.clonefile
clonefile.argtypes = [ctypes.c_char_p, ctypes.c_char_p, ctypes.c_int]
clonefile.restype = ctypes.c_int

if len(sys.argv) != 2:
    raise RuntimeError('Expected one package copy plan file')
with open(sys.argv[1], encoding='utf-8') as plan:
    entries = json.load(plan)

for entry in entries:
    destination = entry['destination']
    if entry['type'] == 'directory':
        os.makedirs(destination, mode=entry['mode'], exist_ok=True)
        if os.path.islink(destination) or not os.path.isdir(destination):
            raise RuntimeError('Package directory is not plain: ' + destination)
        os.chmod(destination, entry['mode'])
        continue
    if entry['type'] == 'file' and os.path.exists(destination) and os.path.samefile(entry['source'], destination):
        raise RuntimeError('Package copy source and destination are identical: ' + destination)
    if os.path.lexists(destination):
        if stat.S_ISDIR(os.lstat(destination).st_mode):
            raise RuntimeError('Package file destination is a directory: ' + destination)
        os.unlink(destination)
    if entry['type'] == 'symlink':
        os.symlink(entry['target'], destination)
    elif entry['type'] == 'file':
        source = entry['source']
        if not stat.S_ISREG(os.lstat(source).st_mode):
            raise RuntimeError('Package source is not a regular file: ' + source)
        if clonefile(os.fsencode(source), os.fsencode(destination), 0) != 0:
            error = ctypes.get_errno()
            raise OSError(error, os.strerror(error), destination)
    else:
        raise RuntimeError('Unknown package copy entry')
