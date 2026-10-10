import { useEffect, useRef, useState, type CSSProperties } from "react";

export const powerFieldContract = {
  colors: [[.592,.388,.945],[.831,.710,.953],[.286,0,.404],[.145,.055,.478],[.592,0,.996],[.780,.459,.914],[.725,.576,1],[.400,.212,.820],[.882,.690,1],[.498,.345,.957],[.659,.275,.910],[.212,.063,.400]],
  centers: [[.18,.36,.18,.42],[.34,.62,.24,.38],[.52,.30,.28,.36],[.70,.68,.24,.36],[.88,.36,.16,.40],[.12,.72,.20,.30],[.30,.44,.22,.42],[.46,.72,.26,.32],[.60,.26,.28,.38],[.76,.54,.22,.40],[.92,.66,.15,.30],[.06,.32,.14,.40]],
  frequencies: [[.42,.50,.91],[.62,.47,1.07],[.38,.58,.76],[.54,.41,1.18],[.74,.64,1.03],[.48,.70,.83],[.58,.52,1.24],[.68,.56,.96],[.44,.60,1.11],[.50,.66,.72],[.70,.46,1.29],[.40,.60,.88]],
} as const;

export const powerParticleContract = [
 [50,.4049607076318353,.6155471285694603,2,6,1.3038800108712167],
 [87,.5278225205933268,.7398640982323741,1,10,1.1557658734936922],
 [72,.8365567349464982,.9464005545849432,0,10,1.8839509764817195],
 [60,.8981869232346071,.5946276453509199,-2,18,1.5413089261128334],
 [48,.9933723940062918,.6266601204410108,3,13,1.8877628648828249],
 [56,.7179431657015811,.6172033079212269,2,13,1.6257011859444903],
 [18,.7876076511216525,.5217518094936168,0,13,.9451303140085656],
 [42,.9917111948225511,.681737361193882,0,13,1.8134734329010826],
 [29,.7551541048545914,.5797855672814421,0,13,.8706901643628954],
 [49,.7008283564806335,.5637131452545873,-4,10,1.1435013950140274],
 [90,.6140308655114495,.8520660675072578,1,18,1.6893470376569895],
 [8,.8920286974316696,.523427322632051,1,16,1.5584861425973942],
 [13,.7725064914178802,.7585726745473949,-1,8,.9597770823747851],
 [8,.8797534316843667,.5631046581485861,3,15,.8758528573729564],
] as const;

export const powerBurstContract = [[-3,-34,0],[15,-29,8],[30,-19,0],[34,-2,12],[26,20,0],[12,31,10],[-6,34,0],[-22,26,14],[-32,9,0],[-32,-10,8],[-21,-26,0],[7,-24,6],[24,-9,0],[20,10,10],[-9,21,0],[-25,-5,12]] as const;

const vertexShader = [
 "attribute vec2 corner;",
 "varying vec2 location;",
 "void main() { location = (corner + vec2(1.0)) / 2.0; gl_Position = vec4(corner, 0.0, 1.0); }",
].join("\n");
export const powerFragmentShader = [
 "precision highp float;",
 "varying vec2 location;",
 "uniform vec2 size;",
 "uniform float clock;",
 "uniform vec3 palette[12];",
 "uniform vec4 orbit[12];",
 "uniform vec3 frequency[12];",
 "void main() {",
 "  vec3 weighted = vec3(0.0); float mass = 0.0;",
 "  vec2 pixel = vec2(location.x * 1.55, 0.40 + location.y * 0.18);",
 "  for (int i = 0; i < 12; i++) {",
 "    float tx = clock * 1.25 * frequency[i].x;",
 "    float ty = clock * 1.25 * frequency[i].y;",
 "    bool odd = (i == 1 || i == 3 || i == 5 || i == 7 || i == 9 || i == 11);",
 "    vec2 wave = odd ? vec2(cos(tx), sin(ty)) : vec2(sin(tx), cos(ty));",
 "    vec2 center = (orbit[i].xy + orbit[i].zw * wave) * vec2(1.55, 1.0);",
 "    vec2 delta = pixel - center;",
 "    float pulse = odd ? cos(clock * frequency[i].z) : sin(clock * frequency[i].z);",
 "    float weight = exp(-12.0 * dot(delta, delta)) * (0.7 + 0.3 * pulse);",
 "    weighted += weight * palette[i]; mass += weight;",
 "  }",
 "  vec3 color = pow(clamp(mix(palette[3], weighted / max(mass, 0.0001), 0.96), 0.0, 1.0), vec3(0.9));",
 "  vec2 noisePoint = location * size * 0.5 + vec2(clock);",
 "  float noise = fract(sin(dot(noisePoint, vec2(12.9898, 78.233))) * 43758.5453);",
 "  color += (noise * 2.0 - 1.0) * 0.012;",
 "  color += vec3(sin(clock * 0.5), cos(clock * 0.7), sin(clock * 0.6)) * 0.02;",
 "  gl_FragColor = vec4(pow(color, vec3(0.92)), 1.0);",
 "}",
].join("\n");

function PowerField({ reducedMotion, active }: { reducedMotion: boolean; active: boolean }) {
  const canvas = useRef<HTMLCanvasElement>(null);
  useEffect(() => {
    const element = canvas.current;
    if (!element || !active) return;
    const gl = element.getContext("webgl", { alpha: true, depth: false, stencil: false, antialias: false, powerPreference: "high-performance" });
    if (!gl) { element.dataset.rendering = "unavailable"; return; }
    const program = gl.createProgram(), buffer = gl.createBuffer();
    if (!program || !buffer) { if (program) gl.deleteProgram(program); if (buffer) gl.deleteBuffer(buffer); element.dataset.rendering = "unavailable"; return; }
    const shaders: WebGLShader[] = [];
    let frame = 0, observer: ResizeObserver | null = null;
    const cleanup = () => {
      cancelAnimationFrame(frame); observer?.disconnect();
      for (const shader of shaders) gl.deleteShader(shader);
      gl.deleteBuffer(buffer); gl.deleteProgram(program);
    };
    for (const [kind, source] of [[gl.VERTEX_SHADER, vertexShader], [gl.FRAGMENT_SHADER, powerFragmentShader]] as const) {
      const shader = gl.createShader(kind);
      if (!shader) { cleanup(); element.dataset.rendering = "unavailable"; return; }
      shaders.push(shader); gl.shaderSource(shader, source); gl.compileShader(shader);
      if (!gl.getShaderParameter(shader, gl.COMPILE_STATUS)) { cleanup(); element.dataset.rendering = "unavailable"; return; }
      gl.attachShader(program, shader);
    }
    gl.linkProgram(program);
    if (!gl.getProgramParameter(program, gl.LINK_STATUS)) { cleanup(); element.dataset.rendering = "unavailable"; return; }
    gl.useProgram(program);
    gl.bindBuffer(gl.ARRAY_BUFFER, buffer);
    gl.bufferData(gl.ARRAY_BUFFER, new Float32Array([-1,-1,1,-1,-1,1,-1,1,1,-1,1,1]), gl.STATIC_DRAW);
    const corner = gl.getAttribLocation(program, "corner");
    gl.enableVertexAttribArray(corner); gl.vertexAttribPointer(corner, 2, gl.FLOAT, false, 0, 0);
    gl.uniform3fv(gl.getUniformLocation(program, "palette[0]"), new Float32Array(powerFieldContract.colors.flat()));
    gl.uniform4fv(gl.getUniformLocation(program, "orbit[0]"), new Float32Array(powerFieldContract.centers.flat()));
    gl.uniform3fv(gl.getUniformLocation(program, "frequency[0]"), new Float32Array(powerFieldContract.frequencies.flat()));
    const started = performance.now(), clock = gl.getUniformLocation(program, "clock");
    const draw = (now: number) => { gl.uniform1f(clock, reducedMotion ? 0 : (now - started) / 1000); gl.drawArrays(gl.TRIANGLES, 0, 6); };
    const resize = () => {
      const { width, height } = element.getBoundingClientRect(), scale = Math.min(window.devicePixelRatio, 2);
      const w = Math.max(1, Math.round(width)), h = Math.max(1, Math.round(height));
      element.width = Math.round(w * scale); element.height = Math.round(h * scale);
      gl.viewport(0, 0, element.width, element.height); gl.uniform2f(gl.getUniformLocation(program, "size"), w, h);
      draw(performance.now());
    };
    observer = new ResizeObserver(resize); observer.observe(element); resize();
    const loop = (now: number) => { draw(now); frame = requestAnimationFrame(loop); };
    if (!reducedMotion) frame = requestAnimationFrame(loop);
    element.dataset.rendering = "webgl";
    return cleanup;
  }, [reducedMotion, active]);
  return <span className="provider-power-field"><canvas ref={canvas} aria-hidden="true" /></span>;
}

function PowerParticle({ index, active }: { index: number; active: boolean }) {
  const [left, opacity, scale, initialX, initialY, initialDuration] = powerParticleContract[index];
  const [position, setPosition] = useState<{ x: number; y: number; duration: number }>({ x: initialX, y: initialY, duration: initialDuration * 1.6 });
  useEffect(() => {
    if (!active) return;
    let timer: ReturnType<typeof setTimeout> | undefined;
    const move = () => {
      const x = Math.random(), y = Math.random(), duration = (.8 + Math.random() * 1.2) * 1.6;
      setPosition({ x: Math.round((x - .5) * 8), y: Math.round(12 + (y - .5) * 14), duration });
      timer = setTimeout(move, duration * 1000);
    };
    const frame = requestAnimationFrame(move);
    return () => { cancelAnimationFrame(frame); clearTimeout(timer); };
  }, [active]);
  return <span className="provider-power-particle" style={{
    left: "calc(" + left + "% + " + position.x + "px)", top: position.y,
    opacity, transform: "translate(-50%, -50%) scale(" + scale + ")", transitionDuration: position.duration + "s",
  }} />;
}

export function PowerDecoration({ reducedMotion, reveal, active }: { reducedMotion: boolean; reveal: boolean; active: boolean }) {
  return <span className="provider-power-decoration" data-reveal={reveal && !reducedMotion || undefined} aria-hidden="true">
    <PowerField reducedMotion={reducedMotion} active={active} />
    {!reducedMotion ? <span className="provider-power-particles">{powerParticleContract.map((_, index) => <PowerParticle index={index} key={index} active={active} />)}</span> : null}
  </span>;
}

export function PowerBurst() {
  return <span className="provider-power-burst" aria-hidden="true">
    {powerBurstContract.map(([x,y,delay], index) => <span key={index} style={{
      "--burst-x": x + "px", "--burst-y": y + "px", animationDelay: delay + "ms",
      "--burst-blue": index % 4 === 2 ? "18%" : index % 4 === 0 && index !== 0 ? "32%" : "0%",
    } as CSSProperties} />)}
  </span>;
}
