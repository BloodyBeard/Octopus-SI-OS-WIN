#![cfg(unix)]

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../../..")
}

#[test]
fn telegram_binding_contract_covers_streams_media_mentions_and_indexed_delivery() {
    let dir = tempfile::tempdir().expect("temp root");
    let var_dir = dir.path().join("var");
    let blobs_dir = dir.path().join("blobs");
    let movements = dir.path().join("movements.jsonl");
    let index = dir.path().join("ledger-index.sqlite");
    std::fs::create_dir_all(&var_dir).expect("var directory creates");
    std::fs::set_permissions(&var_dir, std::fs::Permissions::from_mode(0o700))
        .expect("interface private state directory mode sets");
    std::fs::create_dir_all(&blobs_dir).expect("blob directory creates");
    std::fs::write(&movements, b"").expect("movement log creates");

    let script = r#"
import copy, importlib.util, json, os, sqlite3, sys
p = 'registry/interface/world_telegram/telegram_life.py'
sys.path.insert(0, os.path.dirname(p))
from octopus_content_hash import octopus_content_hash_reference
assert octopus_content_hash_reference(b'abc') == '6437b3ac38465133ffb63b75273a8db548c558465d79db03fd359c6cd5bd9d85'
s = importlib.util.spec_from_file_location('telegram_life', p)
m = importlib.util.module_from_spec(s)
s.loader.exec_module(m)
m.octopus_content_hash = octopus_content_hash_reference
c = m.load_config()
c = copy.deepcopy(c)
c['mode'] = 'binding'
c.pop('gateway_run_template', None)
secret_dir = os.path.join(os.environ['OCTOPUS_VAR_DIR'], 'binding-secrets')
os.makedirs(secret_dir, mode=0o700, exist_ok=True)
os.chmod(secret_dir, 0o700)
secret_handle = os.path.join(secret_dir, 'contract.key')
with open(secret_handle, 'wb') as handle:
    handle.write(b'a' * 32)
os.chmod(secret_handle, 0o600)
c['binding'] = {
  'interface': 'world_telegram_private',
  'chat_id': 4242,
  'thread_id': None,
  'gateway_interface': 'world_telegram',
  'secret_handle': secret_handle,
  'host': '127.0.0.1',
  'port': 31337,
  'run': 'run_private',
}
m._validate_config(c)
bad = copy.deepcopy(c)
bad['outbound']['unknown'] = True
try:
    m._validate_config(bad)
    raise AssertionError('unknown config field was accepted')
except ValueError:
    pass

class Api:
    def __init__(self):
        self.calls = []
        self.next_id = 100
    def _result(self, method, params):
        self.calls.append((method, params))
        self.next_id += 1
        if method == 'sendMediaGroup':
            return [{'message_id': self.next_id - 1}, {'message_id': self.next_id}]
        return {'message_id': self.next_id}
    def call(self, method, params=None, timeout_s=None):
        return self._result(method, params or {})
    def call_flood_safe(self, method, params, config):
        return self._result(method, params)
    def call_multipart_flood_safe(self, method, params, files, config):
        assert sum(len(spec[2]) for spec in files.values()) <= config['media']['max_multipart_bytes']
        return self._result(method, params)

api = Api()
m.load_or_create_chat_state(4242, None)
m.update_chat_state(4242, None, last_user_message_id=55)
reply, result, updates = m.prepare_interface_output(c, 'agent.message.delta', json.dumps({'seq': 0, 'text': 'hello'}).encode())
first = m.render_reply(c, api, json.dumps(reply).encode(), c['binding'])
m.update_chat_state(4242, None, **updates)
assert first['message_id'] == 101
final_text = 'z' * 5000
reply, result, updates = m.prepare_interface_output(c, 'interface.artifact.output', json.dumps({'text': final_text}).encode())
last = m.render_reply(c, api, json.dumps(reply).encode(), c['binding'])
m.update_chat_state(4242, None, **updates)
assert last['message_id'] > first['message_id']
assert any(method == 'editMessageText' for method, _ in api.calls)
for method, params in api.calls:
    if method in ('sendMessage', 'editMessageText'):
        assert m.telegram_text_units(params['text']) <= c['outbound']['max_message_len']
assert m.read_chat_state(4242, None)['stream_message_id'] is None

payload_a = b'a' * 10
payload_b = b'b' * 10
blob_a = octopus_content_hash_reference(payload_a)
blob_b = octopus_content_hash_reference(payload_b)
assert len(blob_a) == 64 and len(blob_b) == 64
open(os.path.join(m.BLOBS_DIR, blob_a), 'wb').write(payload_a)
open(os.path.join(m.BLOBS_DIR, blob_b), 'wb').write(payload_b)
before = len(api.calls)
native = {
  'media': [
    {'kind': 'photo', 'blob_id': blob_a, 'filename': 'a.jpg'},
    {'kind': 'video', 'blob_id': blob_b, 'filename': 'b.mp4'},
  ],
  'caption': '*' * 1300,
}
m.render_reply(c, api, json.dumps(native).encode(), c['binding'])
new_calls = api.calls[before:]
assert any(method == 'sendMediaGroup' for method, _ in new_calls)
group = next(params for method, params in new_calls if method == 'sendMediaGroup')
assert m.telegram_text_units(group['media'][0]['caption']) <= c['outbound']['max_caption_len']
assert any(method == 'sendMessage' for method, _ in new_calls)

before = len(api.calls)
mixed = {
  'media': [
    {'kind': 'photo', 'telegram_file_id': 'photo-file'},
    {'kind': 'voice', 'telegram_file_id': 'voice-file'},
  ]
}
m.render_reply(c, api, json.dumps(mixed).encode(), c['binding'])
methods = [method for method, _ in api.calls[before:]]
assert methods.count('sendPhoto') == 1 and methods.count('sendVoice') == 1
assert 'sendMediaGroup' not in methods

m._RESOLVED_BOT_USERNAME = 'octopus_bot'
message = {
  'chat': {'id': -1001, 'type': 'supergroup'},
  'text': '😀 @octopus_bot hello',
  'entities': [{'type': 'mention', 'offset': 3, 'length': 12}],
}
assert m.should_respond(c, message)

db = sqlite3.connect(m.LEDGER_INDEX_SQLITE)
db.executescript("CREATE TABLE index_meta(key TEXT PRIMARY KEY, value TEXT NOT NULL); INSERT INTO index_meta VALUES ('schema_version','3'); CREATE TABLE movements(seq INTEGER PRIMARY KEY, lamport INTEGER, recorded_at_unix_ms INTEGER, trace TEXT, kind TEXT, mark TEXT, from_kind TEXT, from_id TEXT, to_kind TEXT, to_id TEXT, artifact_id TEXT, artifact_label TEXT, artifact_content TEXT, runtime_run TEXT, runtime_checkpoint TEXT, runtime_interface TEXT);")
db.execute('INSERT INTO movements VALUES (?,?,?,?,?,?,?,?,?,?,?,?,?,?,?,?)', (9, 9, 1, 'trace', 'InterfaceOutbound', None, 'interface', 'world_provider_private', 'interface', 'world_telegram_private', 'artifact', 'interface.artifact.output', 'ab' * 32, 'run_private', 'cp', 'world_telegram_private'))
db.commit()
db.close()
assert m.read_own_outbound(0) == [(9, bytes.fromhex('ab' * 32), 'run_private')]

try:
    m.render_reply(c, api, json.dumps({'media': [{'kind': 'unknown', 'telegram_file_id': 'x'}]}).encode(), c['binding'])
    raise AssertionError('unknown media kind was accepted')
except ValueError:
    pass
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(script)
        .current_dir(repo_root())
        .env("OCTOPUS_INTERFACE_ID", "world_telegram_private")
        .env(
            "OCTOPUS_INTERFACE_DIR",
            repo_root().join("registry/interface/world_telegram"),
        )
        .env("OCTOPUS_VAR_DIR", &var_dir)
        .env("OCTOPUS_MOVEMENTS_JSONL", &movements)
        .env("OCTOPUS_BLOBS_DIR", &blobs_dir)
        .env("OCTOPUS_LEDGER_INDEX_SQLITE", &index)
        .output()
        .expect("telegram binding contract runs");
    assert!(
        output.status.success(),
        "telegram binding contract failed:\nstdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
