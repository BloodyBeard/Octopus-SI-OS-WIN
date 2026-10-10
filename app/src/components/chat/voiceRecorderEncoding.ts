export type EncodedVoiceRecording = {
  bytes: ArrayBuffer;
  durationMs: number;
};

const LITTLE_ENDIAN = new Uint8Array(Uint16Array.of(1).buffer)[0] === 1;

export function encodeVoiceRecording(
  chunks: readonly Float32Array[],
  inputSampleRateHz: number,
  outputSampleRateHz: number,
): EncodedVoiceRecording | null {
  if (
    !Number.isFinite(inputSampleRateHz)
    || inputSampleRateHz <= 0
    || !Number.isFinite(outputSampleRateHz)
    || outputSampleRateHz <= 0
  ) return null;

  const inputSampleCount = chunks.reduce((count, chunk) => count + chunk.length, 0);
  if (!inputSampleCount) return null;

  const inputSamplesPerOutputSample = inputSampleRateHz / outputSampleRateHz;
  const outputSampleCount = Math.max(1, Math.round(inputSampleCount / inputSamplesPerOutputSample));
  const view = new DataView(new ArrayBuffer(44 + outputSampleCount * 2));
  writeWavHeader(view, outputSampleCount, outputSampleRateHz);
  const pcm = LITTLE_ENDIAN ? new Int16Array(view.buffer, 44, outputSampleCount) : null;

  let chunkIndex = 0;
  let chunkStart = 0;
  let chunk = chunks[0];
  let byteOffset = 44;
  for (let index = 0; index < outputSampleCount; index += 1) {
    const sourceIndex = index * inputSamplesPerOutputSample;
    const leftIndex = Math.floor(sourceIndex);
    while (chunk && leftIndex >= chunkStart + chunk.length && chunkIndex < chunks.length - 1) {
      chunkStart += chunk.length;
      chunkIndex += 1;
      chunk = chunks[chunkIndex];
    }
    const localIndex = leftIndex - chunkStart;
    const left = chunk?.[localIndex] ?? 0;
    const right = leftIndex >= inputSampleCount - 1
      ? left
      : chunk?.[localIndex + 1] ?? chunks[chunkIndex + 1]?.[0] ?? left;
    const sample = inputSampleRateHz === outputSampleRateHz
      ? left
      : Math.fround(left + (right - left) * (sourceIndex - leftIndex));
    const clamped = Math.max(-1, Math.min(1, sample));
    const encoded = Math.round(clamped < 0 ? clamped * 0x8000 : clamped * 0x7fff);
    if (pcm) pcm[index] = encoded;
    else view.setInt16(byteOffset, encoded, true);
    byteOffset += 2;
  }

  return {
    bytes: view.buffer,
    durationMs: Math.max(1, Math.round(outputSampleCount / outputSampleRateHz * 1_000)),
  };
}

function writeWavHeader(view: DataView, sampleCount: number, sampleRateHz: number) {
  writeAscii(view, 0, "RIFF");
  view.setUint32(4, 36 + sampleCount * 2, true);
  writeAscii(view, 8, "WAVE");
  writeAscii(view, 12, "fmt ");
  view.setUint32(16, 16, true);
  view.setUint16(20, 1, true);
  view.setUint16(22, 1, true);
  view.setUint32(24, sampleRateHz, true);
  view.setUint32(28, sampleRateHz * 2, true);
  view.setUint16(32, 2, true);
  view.setUint16(34, 16, true);
  writeAscii(view, 36, "data");
  view.setUint32(40, sampleCount * 2, true);
}

function writeAscii(view: DataView, offset: number, value: string) {
  for (let index = 0; index < value.length; index += 1) {
    view.setUint8(offset + index, value.charCodeAt(index));
  }
}
