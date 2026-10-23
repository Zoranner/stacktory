import * as THREE from "three";
import { EffectComposer } from "three/addons/postprocessing/EffectComposer.js";
import { RenderPass } from "three/addons/postprocessing/RenderPass.js";
import { UnrealBloomPass } from "three/addons/postprocessing/UnrealBloomPass.js";
import { OutputPass } from "three/addons/postprocessing/OutputPass.js";

const canvas = document.getElementById("blackhole-canvas");

if (canvas) {
  function showFallback() {
    canvas.classList.add("is-fallback");
    canvas.parentElement?.classList.add("is-fallback");
  }

  const reduceMotion = window.matchMedia(
    "(prefers-reduced-motion: reduce)",
  ).matches;
  const isCoarsePointer = window.matchMedia("(pointer: coarse)").matches;
  const mobile = window.innerWidth < 760;
  const initialYaw = 0.34;
  const initialPitch = mobile ? 0.15 : 0.09;
  const desktopDistance = 12.5;
  const mobileDistance = 16.5;
  const viewDistance = () => (
    window.innerWidth < 760 ? mobileDistance : desktopDistance
  );
  const initialDistance = viewDistance();

  const state = {
    elapsed: 0,
    lastFrame: performance.now(),
    baseYaw: initialYaw,
    pointerX: 0,
    pointerY: 0,
    targetPointerX: 0,
    targetPointerY: 0,
    contextLost: false,
    shaderFailed: false,
  };

  const camera = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1);
  const scene = new THREE.Scene();
  const geometry = new THREE.PlaneGeometry(2, 2);
  const uniforms = {
    uResolution: { value: new THREE.Vector2(window.innerWidth, window.innerHeight) },
    uTime: { value: 0 },
    uYaw: { value: initialYaw },
    uPitch: { value: initialPitch },
    uDistance: { value: initialDistance },
    uFov: { value: 52 },
    uGlow: { value: 1 },
  };

  const fragmentShader = `
    precision highp float;

    #define PI 3.141592653589793
    #define TAU 6.283185307179586
    #define MAX_STEPS __MAX_STEPS__
    #define DISK_INNER 2.08
    #define DISK_OUTER 8.75
    #define HORIZON 1.0

    uniform vec2 uResolution;
    uniform float uTime;
    uniform float uYaw;
    uniform float uPitch;
    uniform float uDistance;
    uniform float uFov;
    uniform float uGlow;

    float hash13(vec3 p3) {
      p3 = fract(p3 * 0.1031);
      p3 += dot(p3, p3.yzx + 33.33);
      return fract((p3.x + p3.y) * p3.z);
    }

    float noise3(vec3 p) {
      vec3 i = floor(p);
      vec3 f = fract(p);
      f = f * f * (3.0 - 2.0 * f);
      float n000 = hash13(i);
      float n100 = hash13(i + vec3(1.0, 0.0, 0.0));
      float n010 = hash13(i + vec3(0.0, 1.0, 0.0));
      float n110 = hash13(i + vec3(1.0, 1.0, 0.0));
      float n001 = hash13(i + vec3(0.0, 0.0, 1.0));
      float n101 = hash13(i + vec3(1.0, 0.0, 1.0));
      float n011 = hash13(i + vec3(0.0, 1.0, 1.0));
      float n111 = hash13(i + vec3(1.0, 1.0, 1.0));
      float nx00 = mix(n000, n100, f.x);
      float nx10 = mix(n010, n110, f.x);
      float nx01 = mix(n001, n101, f.x);
      float nx11 = mix(n011, n111, f.x);
      float nxy0 = mix(nx00, nx10, f.y);
      float nxy1 = mix(nx01, nx11, f.y);
      return mix(nxy0, nxy1, f.z);
    }

    float fbm(vec3 p) {
      float value = 0.0;
      float amplitude = 0.55;
      mat3 rotation = mat3(
        0.80, 0.60, 0.0,
       -0.60, 0.80, 0.0,
        0.0,  0.0, 1.0
      );
      for (int i = 0; i < 4; i++) {
        value += noise3(p) * amplitude;
        p = rotation * p * 2.03 + vec3(7.1, 3.7, 1.9);
        amplitude *= 0.48;
      }
      return value;
    }

    vec3 starLayer(vec3 direction, float density, float seed, float brightness) {
      vec3 p = direction * density;
      vec3 cell = floor(p);
      vec3 local = fract(p) - 0.5;
      float seedValue = hash13(cell + seed);
      float presence = smoothstep(0.875, 0.995, seedValue);
      if (presence <= 0.0) return vec3(0.0);

      vec3 offset = vec3(
        hash13(cell + seed + 2.1),
        hash13(cell + seed + 7.4),
        hash13(cell + seed + 11.8)
      ) - 0.5;
      float distanceToStar = length(local - offset * 0.68);
      float radius = mix(0.018, 0.055, hash13(cell + seed + 17.2));
      float core = 1.0 - smoothstep(0.0, radius, distanceToStar);
      float halo = 1.0 - smoothstep(0.0, radius * 3.6, distanceToStar);
      float twinkle = 0.86 + 0.14 * sin(uTime * 1.2 + seedValue * 31.0);
      float colorMix = hash13(cell + seed + 23.9);
      vec3 color = mix(vec3(0.55, 0.72, 1.0), vec3(1.0, 0.72, 0.43), colorMix);
      color = mix(color, vec3(0.84, 0.92, 1.0), step(0.83, colorMix));
      return color * (core + halo * 0.18) * presence * brightness * twinkle;
    }

    vec3 starField(vec3 direction) {
      vec3 stars = vec3(0.0);
      stars += starLayer(direction, 92.0, 3.1, 1.6);
      stars += starLayer(direction, 185.0, 19.7, 1.05);
      stars += starLayer(direction, 310.0, 41.3, 0.65);
      return stars;
    }

    vec3 background(vec3 direction) {
      vec3 base = vec3(0.0015, 0.0022, 0.0048);

      float cloudA = fbm(direction * 2.2 + vec3(1.8, -0.7, 2.9));
      float cloudB = fbm(direction * 5.1 + vec3(-3.2, 2.1, 0.4));
      float dust = smoothstep(0.36, 0.82, cloudA * 0.76 + cloudB * 0.31);
      float band = exp(-pow(dot(direction, normalize(vec3(0.34, 0.48, -0.42))) / 0.42, 2.0));

      vec3 coldCloud = vec3(0.015, 0.038, 0.061);
      vec3 warmCloud = vec3(0.065, 0.035, 0.023);
      vec3 nebula = mix(coldCloud, warmCloud, smoothstep(0.3, 0.9, cloudB));
      base += nebula * dust * (0.34 + band * 0.8);
      base += vec3(0.011, 0.012, 0.016) * band * (0.55 + cloudA * 0.6);

      vec3 stars = starField(direction);
      base += stars;
      return base;
    }

    void diskSample(vec3 hit, vec3 rayOrigin, inout vec3 accumulation, inout float opacity) {
      float radius = length(hit.xz);
      if (radius < DISK_INNER || radius > DISK_OUTER) return;

      float t = (radius - DISK_INNER) / (DISK_OUTER - DISK_INNER);
      float angle = atan(hit.z, hit.x);
      float angularVelocity = 2.5 / pow(radius, 1.28);
      float swirl = angle + uTime * angularVelocity;
      vec3 orbitPoint = vec3(cos(swirl), sin(swirl), 0.0) * (radius * 1.08);
      vec3 noisePoint = orbitPoint + vec3(0.0, 0.0, radius * 1.72 - uTime * 0.09);

      float cloud = fbm(noisePoint * 0.72);
      float fine = fbm(noisePoint * 2.55 + vec3(5.2, 1.6, -2.7));
      float bands = 0.5 + 0.5 * sin(radius * 21.0 - swirl * 2.0 + cloud * 7.2);
      float filaments = smoothstep(0.18, 0.73, cloud * 0.88 + fine * 0.54);
      float radialFade = smoothstep(0.0, 0.055, t) * (1.0 - smoothstep(0.56, 1.0, t));
      float density = radialFade * mix(0.2, 1.12, filaments) * (0.55 + bands * 0.56);

      vec3 tangent = normalize(vec3(hit.z, 0.0, -hit.x));
      vec3 toCamera = normalize(rayOrigin - hit);
      float orbitalSpeed = 0.38 * sqrt(DISK_INNER / max(radius, DISK_INNER));
      float approach = dot(tangent, toCamera);
      float beaming = clamp(pow(1.0 / max(0.24, 1.0 - orbitalSpeed * approach), 2.2), 0.18, 3.8);
      float hotSide = smoothstep(-0.72, 0.88, approach);

      float heat = 1.0 - smoothstep(DISK_INNER + 0.3, DISK_OUTER, radius);
      vec3 ember = vec3(0.16, 0.012, 0.0015);
      vec3 amber = vec3(0.82, 0.22, 0.014);
      vec3 whiteHot = vec3(1.9, 1.2, 0.55);
      vec3 color = mix(ember, amber, 1.0 - smoothstep(0.05, 0.72, t));
      color = mix(color, whiteHot, pow(heat, 2.4) * 0.44);

      color *= (0.22 + density * 1.55) * mix(0.5, 1.48, beaming) * uGlow;
      color = mix(color, color * vec3(0.7, 0.9, 1.32), hotSide * 0.32);

      float innerRim = exp(-pow((radius - DISK_INNER) / 0.2, 2.0));
      color += vec3(1.55, 0.92, 0.38) * innerRim * (0.24 + filaments * 0.7) * 0.72 * uGlow;

      float alpha = clamp(density * 0.78, 0.0, 0.94);
      accumulation += color * alpha * (1.0 - opacity);
      opacity += alpha * (1.0 - opacity);
    }

    vec3 cameraRay(vec2 uv, out vec3 rayOrigin) {
      float aspect = uResolution.x / uResolution.y;
      vec2 p = uv * 2.0 - 1.0;
      p.x *= aspect;

      float cp = cos(uPitch);
      rayOrigin = vec3(
        sin(uYaw) * cp,
        sin(uPitch),
        cos(uYaw) * cp
      ) * uDistance;

      vec3 forward = normalize(-rayOrigin);
      vec3 right = normalize(cross(forward, vec3(0.0, 1.0, 0.0)));
      vec3 up = cross(right, forward);
      float tanHalfFov = tan(radians(uFov) * 0.5);
      return normalize(forward + right * p.x * tanHalfFov + up * p.y * tanHalfFov);
    }

    void main() {
      vec2 uv = gl_FragCoord.xy / uResolution;
      vec3 rayOrigin;
      vec3 rayDirection = cameraRay(uv, rayOrigin);
      float impactParameter = length(cross(rayOrigin, rayDirection));
      vec2 centeredUv = (uv - vec2(0.5)) * vec2(uResolution.x / uResolution.y, 1.0);
      float focus = 1.0 - smoothstep(0.1, 0.48, length(centeredUv));

      vec3 pos = rayOrigin;
      vec3 velocity = rayDirection;
      float h2 = dot(cross(pos, velocity), cross(pos, velocity));
      vec3 accumulated = vec3(0.0);
      float opacity = 0.0;
      float minRadius = 100.0;
      bool captured = false;

      for (int i = 0; i < MAX_STEPS; i++) {
        float radius = length(pos);
        minRadius = min(minRadius, radius);
        if (radius < HORIZON) {
          captured = true;
          break;
        }
        if (radius > 42.0) {
          break;
        }

        vec3 gravity = -1.5 * h2 * pos / pow(max(radius, 0.18), 5.0);
        float stepLength = clamp(0.058 * (radius - 0.42), 0.012, 0.5);
        vec3 nextVelocity = velocity + gravity * stepLength;
        vec3 nextPos = pos + (velocity + nextVelocity) * (0.5 * stepLength);

        if (pos.y * nextPos.y <= 0.0 && abs(nextPos.y - pos.y) > 0.00001) {
          float crossing = clamp(pos.y / (pos.y - nextPos.y), 0.0, 1.0);
          vec3 hit = mix(pos, nextPos, crossing);
          diskSample(hit, rayOrigin, accumulated, opacity);
          if (opacity > 0.985) break;
        }

        pos = nextPos;
        velocity = nextVelocity;
      }

      vec3 sky = vec3(0.0);
      if (!captured) {
        sky = background(normalize(velocity));
      }

      vec3 color = accumulated + sky * (1.0 - opacity);

      if (!captured) {
        float photonRing = exp(-pow((impactParameter - 2.598) / 0.055, 2.0));
        color += vec3(1.45, 1.04, 0.66) * photonRing * 0.24 * uGlow;
      }

      float vignette = 1.0 - smoothstep(0.26, 1.13, length(uv - 0.5) * 1.43);
      color *= mix(0.66, 1.0, vignette);

      float grain = hash13(vec3(gl_FragCoord.xy, floor(uTime * 24.0))) - 0.5;
      color += grain * 0.0055 * (0.35 + focus);

      gl_FragColor = vec4(max(color, 0.0), 1.0);
    }
  `;

  const shaderSteps = window.innerWidth < 760 ? 112 : 156;
  let renderer;
  let composer;

  try {
    renderer = new THREE.WebGLRenderer({
      canvas,
      antialias: false,
      alpha: false,
      powerPreference: "high-performance",
    });
    renderer.setClearColor(0x050608, 1);
    renderer.outputColorSpace = THREE.SRGBColorSpace;
    renderer.toneMapping = THREE.ACESFilmicToneMapping;
    renderer.toneMappingExposure = 0.82;
    renderer.debug.checkShaderErrors = true;
    renderer.debug.onShaderError = (gl, program, vertexShader, fragmentShader) => {
      const programLog = gl.getProgramInfoLog(program) ?? "";
      const vertexLog = gl.getShaderInfoLog(vertexShader) ?? "";
      const fragmentLog = gl.getShaderInfoLog(fragmentShader) ?? "";
      console.error("Black hole shader error.", {
        programLog,
        vertexLog,
        fragmentLog,
      });
      state.shaderFailed = true;
      showFallback();
    };

    const pixelRatio = Math.min(
      window.devicePixelRatio || 1,
      isCoarsePointer ? 1.1 : 1.3,
    );
    renderer.setPixelRatio(pixelRatio);
    renderer.setSize(window.innerWidth, window.innerHeight, false);

    const material = new THREE.ShaderMaterial({
      uniforms,
      vertexShader: `
        void main() {
          gl_Position = vec4(position.xy, 0.0, 1.0);
        }
      `,
      fragmentShader: fragmentShader.replace(
        "__MAX_STEPS__",
        String(shaderSteps),
      ),
      depthWrite: false,
      depthTest: false,
      toneMapped: false,
    });
    scene.add(new THREE.Mesh(geometry, material));

    composer = new EffectComposer(renderer);
    composer.setPixelRatio(pixelRatio);
    composer.setSize(window.innerWidth, window.innerHeight);
    composer.addPass(new RenderPass(scene, camera));
    composer.addPass(new UnrealBloomPass(
      new THREE.Vector2(window.innerWidth, window.innerHeight),
      mobile ? 0.52 : 0.62,
      0.68,
      0.55,
    ));
    composer.addPass(new OutputPass());
  } catch (error) {
    showFallback();
    console.error("Black hole scene unavailable", error);
    throw error;
  }

  function resize() {
    const width = window.innerWidth;
    const height = window.innerHeight;
    const mobileViewport = width < 760;
    const pixelRatio = renderer.getPixelRatio();

    renderer.setSize(width, height, false);
    composer.setSize(width, height);
    uniforms.uResolution.value.set(width * pixelRatio, height * pixelRatio);
    uniforms.uPitch.value = mobileViewport ? 0.15 : 0.09;
    uniforms.uDistance.value = viewDistance();
    state.lastFrame = performance.now();

    if (reduceMotion && !state.contextLost && !state.shaderFailed) {
      composer.render();
    }
  }

  function animate(now) {
    if (state.contextLost || state.shaderFailed) {
      return;
    }

    requestAnimationFrame(animate);

    const delta = Math.min((now - state.lastFrame) / 1000, 0.05);
    state.lastFrame = now;
    state.elapsed += delta;

    const smoothing = 1 - Math.exp(-delta * 3.6);
    state.pointerX += (state.targetPointerX - state.pointerX) * smoothing;
    state.pointerY += (state.targetPointerY - state.pointerY) * smoothing;

    if (!reduceMotion) {
      state.baseYaw += delta * 0.018;
    }

    uniforms.uTime.value = state.elapsed;
    uniforms.uYaw.value = state.baseYaw + state.pointerX * 0.16;
    uniforms.uPitch.value = Math.max(
      0.02,
      Math.min(
        0.28,
        (window.innerWidth < 760 ? 0.15 : 0.09) + state.pointerY * 0.07,
      ),
    );
    uniforms.uDistance.value = viewDistance();

    composer.render();
  }

  window.addEventListener("resize", resize);

  window.addEventListener("pointermove", (event) => {
    if (event.pointerType === "touch") {
      return;
    }
    state.targetPointerX = Math.max(
      -1,
      Math.min(1, (event.clientX / Math.max(1, window.innerWidth)) * 2 - 1),
    );
    state.targetPointerY = Math.max(
      -1,
      Math.min(1, (event.clientY / Math.max(1, window.innerHeight)) * 2 - 1),
    );
  }, { passive: true });

  window.addEventListener("pointerout", (event) => {
    if (event.relatedTarget) {
      return;
    }
    state.targetPointerX = 0;
    state.targetPointerY = 0;
  });

  window.addEventListener("blur", () => {
    state.targetPointerX = 0;
    state.targetPointerY = 0;
  });

  canvas.addEventListener("webglcontextlost", (event) => {
    event.preventDefault();
    state.contextLost = true;
    showFallback();
  });

  resize();
  requestAnimationFrame(animate);
}
