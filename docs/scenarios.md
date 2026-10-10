# Scenarios

Two design examples show how Octopus can acquire a capability and change what
it listens for. They describe the intended use of the architecture, rather
than tested, ready-to-use features. Each depends on a model doing the work,
the necessary permissions, and checks of the resulting behavior.

Read [Architecture](architecture.md) first.

## 1. Adding a camera at runtime

1. You say, out loud, "what should I wear with this?" Sound reaches a
   microphone interface. A World interface is not only an API client - a local
   script that talks to audio hardware is one too. What passes its threshold
   becomes an audio artifact.
2. The artifact arrives at a checkpoint. The run uses the worker named in its
   record and reads only the context its bindings admit.
3. It cannot see you. Its grant has no camera and no way to draw on your
   screen. Its policy does let it author a run for a coder, so it
   does, with a task and checkpoint requirements describing the artifacts the
   coder must produce and the verifiers that must check them.
4. The contract it just authored *is* the plan: what must arrive, and which
   verifier must pass it. Planning needs no separate plan object or engine.
5. The coder, using granted file and execution tools, writes a camera driver
   and registers it by creating a new interface folder. It writes a renderer
   for an overlay window and registers that as another. Both are World
   interfaces because they cross the boundary to your hardware and your screen.
6. The coder sends the evidence its checkpoints require. A passing verifier
   lets the runtime record a mark for that requirement. Checking descriptor
   syntax alone does not prove the camera or overlay works.
7. The new descriptors are picked up live. With the interfaces bound, their
   processes available, and camera access granted, a run takes a frame,
   passes it to a vision-capable provider interface, and sends the answer to
   the overlay.

Once the capability works, later runs can reuse it through their own bindings
and permissions. The addition consists of interface descriptors and the code
behind them; it needs no camera-specific branch in the kernel.

## 2. Choosing its own wake word

1. You join a voice channel with a friend. Audio reaches a microphone
   interface; a speech-to-text interface turns it into a transcript artifact.
2. A run reads the transcript and concludes you are in a conversation where an
   always-on assistant would be a nuisance.
3. If policy permits, it authors a task to configure a speech interface with
   a wake word suited to the call. An existing driver may support this through
   configuration alone; a coder adds package code if new behavior is needed.
4. The configuration and required test evidence pass their verifiers. The
   interface is bound to the waiting run and its listener starts.
5. The system tells you which wake word it selected.
6. The run waits at a checkpoint without holding a thread or a tool room.
   The speech interface keeps listening; that work belongs to its process.
7. When the listener detects the wake word, it emits an artifact. The arrival
   resumes the run through its binding under the current policy.

Here the model chooses what should wake the run and expresses that choice
through interface configuration. Whether it chooses a useful trigger and
implements it reliably must be established by running and testing it.

For the package structure behind these examples, see
[Extending Octopus](contracts/EXTENSION_SURFACES.md).
