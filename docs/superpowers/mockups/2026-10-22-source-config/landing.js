(function () {
  "use strict";

  const canvas = document.getElementById("blackhole-canvas");
  const THREE = window.THREE;

  if (!canvas) {
    return;
  }

  function showFallback() {
    canvas.classList.add("is-fallback");
    canvas.parentElement?.classList.add("is-fallback");
  }

  if (!THREE) {
    showFallback();
    return;
  }

  let renderer;
  try {
    renderer = new THREE.WebGLRenderer({
      canvas,
      alpha: false,
      antialias: false,
      powerPreference: "high-performance",
    });
  } catch (error) {
    showFallback();
    console.error("WebGL renderer unavailable", error);
    return;
  }

  renderer.setClearColor(0x000000, 1);
  renderer.outputEncoding = THREE.LinearEncoding;
  renderer.toneMapping = THREE.NoToneMapping;
  renderer.autoClear = false;

  const gl = renderer.getContext();
  const supportsHdr = renderer.capabilities.isWebGL2
    ? Boolean(
      gl.getExtension("EXT_color_buffer_float")
      || gl.getExtension("EXT_color_buffer_half_float"),
    )
    : Boolean(
      gl.getExtension("OES_texture_half_float")
      && gl.getExtension("EXT_color_buffer_half_float"),
    );
  const renderTargetType = supportsHdr
    ? THREE.HalfFloatType
    : THREE.UnsignedByteType;
  const sceneVertex = `
    varying vec2 vUv;

    void main() {
      vUv = uv;
      gl_Position = vec4(position.xy, 0.0, 1.0);
    }
  `;

  const sceneFragment = `
    precision highp float;

    uniform vec2 uResolution;
    uniform float uTime;
    uniform vec3 uCamPos;
    uniform mat3 uCamBasis;
    uniform float uFovFactor;
    uniform float uSteps;
    uniform float uDiskInner;
    uniform float uDiskOuter;
    uniform float uDiskBrightness;
    uniform float uDoppler;
    uniform float uGravRedshift;
    uniform float uSkyBrightness;
    uniform vec2 uPan;

    varying vec2 vUv;

    float hash31(vec3 p) {
      p = fract(p * 0.1031);
      p += dot(p, p.yzx + 33.33);
      return fract((p.x + p.y) * p.z);
    }

    float noise3(vec3 p) {
      vec3 cell = floor(p);
      vec3 local = fract(p);
      local = local * local * (3.0 - 2.0 * local);

      float n000 = hash31(cell);
      float n100 = hash31(cell + vec3(1.0, 0.0, 0.0));
      float n010 = hash31(cell + vec3(0.0, 1.0, 0.0));
      float n110 = hash31(cell + vec3(1.0, 1.0, 0.0));
      float n001 = hash31(cell + vec3(0.0, 0.0, 1.0));
      float n101 = hash31(cell + vec3(1.0, 0.0, 1.0));
      float n011 = hash31(cell + vec3(0.0, 1.0, 1.0));
      float n111 = hash31(cell + vec3(1.0, 1.0, 1.0));

      return mix(
        mix(mix(n000, n100, local.x), mix(n010, n110, local.x), local.y),
        mix(mix(n001, n101, local.x), mix(n011, n111, local.x), local.y),
        local.z
      );
    }

    float fbm3(vec3 p) {
      float value = 0.0;
      float amplitude = 0.5;

      for (int i = 0; i < 2; i++) {
        value += amplitude * noise3(p);
        p = p * 2.03 + vec3(17.1, 9.2, 5.7);
        amplitude *= 0.5;
      }

      return value;
    }

    float bandLimit(float phase, float power) {
      float cyclesPerPixel = fwidth(phase) * 0.159154943;
      float threshold = mix(
        0.20,
        0.08,
        clamp(power / 46.0, 0.0, 1.0)
      );
      return 1.0 - smoothstep(
        threshold,
        threshold + 0.18,
        cyclesPerPixel
      );
    }

    float bandLimitedLine(float phase, float power) {
      return pow(max(0.5 + 0.5 * sin(phase), 0.0), power)
        * bandLimit(phase, power);
    }

    float screenStar(
      vec2 uv,
      float scale,
      float density,
      float seed,
      float coreWidth
    ) {
      vec2 point = uv * scale;
      vec2 cell = floor(point);
      vec2 local = fract(point) - 0.5;
      float presence = hash31(vec3(cell, seed));
      float exists = smoothstep(
        1.0 - density,
        1.0 - density + 0.0012,
        presence
      );

      if (exists <= 0.0001) {
        return 0.0;
      }

      vec2 offset = vec2(
        hash31(vec3(cell, seed + 3.1)),
        hash31(vec3(cell, seed + 8.7))
      ) - 0.5;
      float distanceToStar = length(local - offset * 0.62);
      float core = 1.0 - smoothstep(0.0, coreWidth, distanceToStar);
      float rareTwinkle = smoothstep(0.88, 0.99, presence);
      float twinkleDepth = 0.07 + 0.33 * rareTwinkle;
      float twinkle = 1.0 - twinkleDepth * (
        0.5 + 0.5 * sin(
          uTime * (0.32 + presence * 0.80) + presence * 21.0
        )
      );

      return exists * core * core
        * (0.35 + 0.65 * presence)
        * twinkle;
    }

    vec3 outerStars(vec2 uv) {
      float outerMask = smoothstep(0.54, 0.90, length(uv));

      if (outerMask <= 0.001) {
        return vec3(0.0);
      }

      float smallWarm = screenStar(uv, 58.0, 0.009, 1.7, 0.11);
      float smallCool = screenStar(uv, 31.0, 0.005, 8.1, 0.13);
      float bright = screenStar(uv, 14.0, 0.0013, 4.1, 0.17);

      vec3 color = vec3(1.0, 0.84, 0.58) * smallWarm * 1.15
        + vec3(0.72, 0.82, 1.0) * smallCool * 1.35
        + vec3(1.0, 0.92, 0.76) * bright * 2.40;

      return color * outerMask;
    }

    vec3 skyColor(vec3 direction) {
      vec3 dir = normalize(direction);
      vec3 galaxyAxis = normalize(vec3(0.18, 0.86, -0.48));
      float band = exp(-pow(dot(dir, galaxyAxis) / 0.32, 2.0));
      float dust = fbm3(dir * 4.2 + vec3(2.3, 5.1, 1.7));
      float milky = band * (0.28 + 0.72 * dust);
      float filament = pow(
        0.5 + 0.5 * sin(
          dir.x * 41.0
          + dir.y * 29.0
          + dir.z * 37.0
          + dust * 8.0
        ),
        12.0
      );

      vec3 color = vec3(0.0007, 0.0011, 0.0021);
      color += vec3(0.012, 0.017, 0.030) * milky * 0.28;
      color *= 1.0 - 0.48 * band * smoothstep(0.45, 0.72, dust);
      color += vec3(0.008, 0.011, 0.018) * band * dust * 0.18;
      color += vec3(0.020, 0.012, 0.005) * band * filament * 0.16;
      color *= 0.98 + 0.02 * sin(uTime * 0.22 + 1.1);

      return color * uSkyBrightness;
    }

    vec3 diskPalette(float temperature) {
      float t = clamp(temperature, 0.0, 1.35);
      vec3 color = mix(
        vec3(0.36, 0.060, 0.008),
        vec3(0.98, 0.32, 0.040),
        smoothstep(0.08, 0.52, t)
      );
      color = mix(
        color,
        vec3(1.0, 0.69, 0.25),
        smoothstep(0.48, 0.92, t)
      );
      color = mix(
        color,
        vec3(1.0, 0.91, 0.70),
        smoothstep(0.88, 1.24, t) * 0.72
      );
      return color;
    }

    float diskDensity(float radius, float phi, float radial) {
      float omega = 0.70710678 / pow(max(radius, 0.5), 1.5);
      float phase = phi - omega * uTime * 3.0;
      vec2 orbit = vec2(cos(phase), sin(phase));
      float logRadius = log(max(radius, 0.25));

      float n1 = fbm3(vec3(
        orbit * 2.6,
        logRadius * 3.1 + uTime * 0.018
      ));
      float turbulence = n1;
      float lineAPhase =
        logRadius * 36.0
        + phase * 2.0
        - uTime * 0.46
        + turbulence * 1.4;
      float lineA = bandLimitedLine(lineAPhase, 12.0);
      float lineBPhase =
        logRadius * 64.0
        + phase * 3.0
        - uTime * 0.78
        + turbulence * 2.0
        + 1.3;
      float lineB = bandLimitedLine(lineBPhase, 24.0);
      float lineCPhase =
        logRadius * 104.0
        + phase * 4.0
        - uTime * 1.18
        + turbulence * 2.8
        + 2.7;
      float lineC = bandLimitedLine(lineCPhase, 34.0);
      float lineDPhase =
        logRadius * 160.0
        + phase * 6.0
        - uTime * 1.62
        + turbulence * 3.6
        + 4.1;
      float lineD = bandLimitedLine(lineDPhase, 46.0);
      float lanePhase =
        phase * 3.0
        + logRadius * 9.0
        - uTime * 0.10
        + turbulence * 4.0;
      float lane = mix(
        0.5,
        0.5 + 0.5 * sin(lanePhase),
        bandLimit(lanePhase, 1.0)
      );
      float armPhase =
        phase * 2.0
        - logRadius * 14.0
        + uTime * 0.38
        + turbulence * 3.0;
      float arm = mix(
        0.5,
        0.5 + 0.5 * sin(armPhase),
        bandLimit(armPhase, 1.0)
      );
      arm = pow(arm, 5.0);
      float radialMask = smoothstep(0.0, 0.026, radial)
        * (1.0 - smoothstep(0.72, 1.0, radial));
      float lineField = lineA * 0.48
        + lineB * 0.64
        + lineC * 0.42
        + lineD * 0.24;
      float innerRim = exp(
        -pow((radial - 0.018) / 0.060, 2.0)
      );
      float density = radialMask
        * (0.018 + lineField * (0.74 + 0.26 * turbulence))
        * mix(0.64, 1.0, lane)
        * (0.62 + 0.38 * arm);
      density = max(
        density,
        innerRim * 1.10 * radialMask
      );
      density = pow(density, 1.10);

      return clamp(density, 0.0, 1.0);
    }

    vec3 sampleDisk(vec3 hit, out float alpha) {
      float radius = length(hit.xz);
      float phi = atan(hit.z, hit.x);
      float radial = clamp(
        (radius - uDiskInner) / max(uDiskOuter - uDiskInner, 0.001),
        0.0,
        1.0
      );
      float density = diskDensity(radius, phi, radial);
      float edge = smoothstep(0.0, 0.018, radial)
        * (1.0 - smoothstep(0.86, 1.0, radial));
      density *= edge;

      float beta = min(0.72, sqrt(0.5 / max(radius, 1.2)));
      float gamma = 1.0 / sqrt(max(0.15, 1.0 - beta * beta));
      vec3 tangent = normalize(vec3(-hit.z, 0.0, hit.x));
      float cosTheta = dot(tangent, normalize(uCamPos - hit));
      float doppler = 1.0 / (gamma * (1.0 - beta * cosTheta));
      float gravity = sqrt(max(0.04, 1.0 - 1.0 / max(radius, 1.001)));

      doppler = mix(1.0, doppler, uDoppler);
      gravity = mix(1.0, gravity, uGravRedshift);
      float shift = doppler * gravity;

      float baseTemperature = pow(
        uDiskInner / max(radius, uDiskInner),
        0.72
      );
      float temperature = baseTemperature
        * clamp(pow(max(shift, 0.2), 0.82), 0.35, 2.2);
      float beaming = clamp(pow(max(shift, 0.15), 2.0), 0.12, 3.2);
      float innerGlow = 1.0 + 1.15 * exp(
        -max(radius - uDiskInner, 0.0) * 2.1
      );
      float hotSpot = 0.78 + 0.22 * sin(
        phi * 2.0 + uTime * 0.32 + density * 5.0
      );
      float innerRim = exp(
        -pow((radial - 0.018) / 0.055, 2.0)
      );
      float breath = 0.94 + 0.06 * sin(uTime * 0.42);
      float haloBreath = 0.86 + 0.14 * sin(uTime * 0.30 + 0.8);
      float rightSide = smoothstep(
        -0.15,
        0.72,
        hit.x / max(radius, 1.0)
      );
      float luminance = (0.04 + 3.10 * pow(density, 1.10))
        * (0.36 + 1.18 * beaming)
        * innerGlow
        * hotSpot
        * uDiskBrightness;
      luminance *= breath;
      luminance += innerRim * 3.40 * innerGlow
        * uDiskBrightness * haloBreath;
      luminance *= mix(0.82, 1.28, rightSide);
      vec3 color = diskPalette(0.26 + temperature * 0.88) * luminance;

      alpha = clamp(0.025 + 0.56 * density, 0.0, 0.76) * edge;
      return color;
    }

    vec3 geodesicAcceleration(vec3 position, float h2) {
      float radiusSquared = dot(position, position);
      float radius = sqrt(radiusSquared);
      float falloff = radiusSquared * radiusSquared * max(radius, 0.05);
      return -1.5 * h2 * position / max(falloff, 0.00001);
    }

    void main() {
      vec2 uv = (gl_FragCoord.xy * 2.0 - uResolution) / uResolution.y;
      uv -= uPan;
      vec3 rayDirection = normalize(
        uCamBasis * vec3(uv.x, uv.y, uFovFactor)
      );

      vec3 position = uCamPos;
      vec3 velocity = rayDirection;
      vec3 angularMomentum = cross(position, velocity);
      float h2 = dot(angularMomentum, angularMomentum);
      vec3 color = vec3(0.0);
      float transmittance = 1.0;
      int crossings = 0;
      vec3 previous = position;

      for (int i = 0; i < 260; i++) {
        if (float(i) >= uSteps) {
          break;
        }

        float radius = length(position);
        float stepSize = clamp(0.082 * radius * radius, 0.012, 1.2);

        vec3 k1x = velocity;
        vec3 k1v = geodesicAcceleration(position, h2);
        vec3 k2x = velocity + 0.5 * stepSize * k1v;
        vec3 k2v = geodesicAcceleration(
          position + 0.5 * stepSize * k1x,
          h2
        );
        vec3 k3x = velocity + 0.5 * stepSize * k2v;
        vec3 k3v = geodesicAcceleration(
          position + 0.5 * stepSize * k2x,
          h2
        );
        vec3 k4x = velocity + stepSize * k3v;
        vec3 k4v = geodesicAcceleration(
          position + stepSize * k3x,
          h2
        );

        previous = position;
        position += (stepSize / 6.0)
          * (k1x + 2.0 * k2x + 2.0 * k3x + k4x);
        velocity += (stepSize / 6.0)
          * (k1v + 2.0 * k2v + 2.0 * k3v + k4v);

        if (previous.y * position.y < 0.0) {
          float ratio = clamp(
            previous.y / (previous.y - position.y),
            0.0,
            1.0
          );
          vec3 hit = mix(previous, position, ratio);
          float hitRadius = length(hit.xz);

          if (uDiskBrightness > 0.001
            && crossings < 1
            && hitRadius < uDiskOuter) {
            float alpha;
            vec3 emission = sampleDisk(hit, alpha);
            color += transmittance * emission * alpha;
            transmittance *= 1.0 - alpha;
            crossings++;

            if (transmittance < 0.01) {
              break;
            }
          }
        }

        if (transmittance < 0.01) {
          break;
        }

        float nextRadius = length(position);
        if (nextRadius < 1.0) {
          break;
        }
        if (nextRadius > 60.0 && dot(position, velocity) > 0.0) {
          break;
        }
      }

      if (transmittance > 0.002) {
        color += skyColor(normalize(velocity)) * transmittance;
      }

      color += outerStars(uv);

      gl_FragColor = vec4(color, 1.0);
    }
  `;

  const finalFragment = `
    precision highp float;

    uniform sampler2D uScene;
    uniform vec2 uSceneTexelSize;
    uniform float uVignette;
    uniform vec2 uPan;
    varying vec2 vUv;

    vec3 aces(vec3 color) {
      color *= 0.94;
      return clamp(
        (color * (2.51 * color + 0.03))
        / (color * (2.43 * color + 0.59) + 0.14),
        0.0,
        1.0
      );
    }

    float aaLuma(vec3 color) {
      return log2(1.0 + max(color.r, max(color.g, color.b)) * 2.0);
    }

    vec3 fxaa(vec2 uv) {
      vec3 center = texture2D(uScene, uv).rgb;
      vec3 northWest = texture2D(
        uScene,
        uv + vec2(-1.0, -1.0) * uSceneTexelSize
      ).rgb;
      vec3 northEast = texture2D(
        uScene,
        uv + vec2(1.0, -1.0) * uSceneTexelSize
      ).rgb;
      vec3 southWest = texture2D(
        uScene,
        uv + vec2(-1.0, 1.0) * uSceneTexelSize
      ).rgb;
      vec3 southEast = texture2D(
        uScene,
        uv + vec2(1.0, 1.0) * uSceneTexelSize
      ).rgb;

      float lCenter = aaLuma(center);
      float lNorthWest = aaLuma(northWest);
      float lNorthEast = aaLuma(northEast);
      float lSouthWest = aaLuma(southWest);
      float lSouthEast = aaLuma(southEast);
      float lMin = min(
        lCenter,
        min(min(lNorthWest, lNorthEast), min(lSouthWest, lSouthEast))
      );
      float lMax = max(
        lCenter,
        max(max(lNorthWest, lNorthEast), max(lSouthWest, lSouthEast))
      );

      if (lMax - lMin < 0.07) {
        return center;
      }

      vec2 direction = vec2(
        -((lNorthWest + lNorthEast) - (lSouthWest + lSouthEast)),
        (lNorthWest + lSouthWest) - (lNorthEast + lSouthEast)
      );
      float directionReduce = max(
        (lNorthWest + lNorthEast + lSouthWest + lSouthEast) * 0.125,
        0.0078125
      );
      float inverseDirection = 1.0 / (
        min(abs(direction.x), abs(direction.y)) + directionReduce
      );
      direction = clamp(
        direction * inverseDirection,
        vec2(-8.0),
        vec2(8.0)
      ) * uSceneTexelSize;

      vec3 blendA = 0.5 * (
        texture2D(uScene, uv + direction * (1.0 / 3.0 - 0.5)).rgb
        + texture2D(uScene, uv + direction * (2.0 / 3.0 - 0.5)).rgb
      );
      vec3 blendB = 0.5 * blendA + 0.25 * (
        texture2D(uScene, uv - direction * 0.5).rgb
        + texture2D(uScene, uv + direction * 0.5).rgb
      );
      float blendLuma = aaLuma(blendB);

      return (blendLuma < lMin || blendLuma > lMax) ? blendA : blendB;
    }

    void main() {
      vec2 centered = vUv - (0.5 + uPan * 0.5);
      vec3 color = fxaa(vUv) * 0.98;
      color = aces(color);

      vec2 warmPoint = vec2(
        (vUv.x - 0.98) * 1.18,
        (vUv.y - 0.46) * 1.20
      );
      float warmGlow = exp(-dot(warmPoint, warmPoint) * 7.2);
      color += vec3(0.014, 0.0042, 0.0009) * warmGlow;

      float vignette = 1.0 - uVignette * smoothstep(
        0.26,
        0.82,
        length(centered * vec2(1.04, 1.0))
      );
      color *= max(0.0, vignette);

      color = pow(max(color, 0.0), vec3(1.0 / 2.2));

      gl_FragColor = vec4(color, 1.0);
    }
  `;

  const quadScene = new THREE.Scene();
  const quadCamera = new THREE.OrthographicCamera(-1, 1, 1, -1, 0, 1);
  const quadGeometry = new THREE.PlaneGeometry(2, 2);
  const quad = new THREE.Mesh(quadGeometry, null);
  quad.frustumCulled = false;
  quadScene.add(quad);

  const sceneUniforms = {
    uResolution: { value: new THREE.Vector2(1, 1) },
    uTime: { value: 0 },
    uCamPos: { value: new THREE.Vector3() },
    uCamBasis: { value: new THREE.Matrix3() },
    uFovFactor: { value: 1 / Math.tan((46 * Math.PI / 180) / 2) },
    uSteps: { value: 170 },
    uDiskInner: { value: 3.0 },
    uDiskOuter: { value: 10.5 },
    uDiskBrightness: { value: 1.08 },
    uDoppler: { value: 1.0 },
    uGravRedshift: { value: 1.0 },
    uSkyBrightness: { value: 1.0 },
    uPan: { value: new THREE.Vector2() },
  };

  const finalUniforms = {
    uScene: { value: null },
    uSceneTexelSize: { value: new THREE.Vector2(1, 1) },
    uVignette: { value: 0.28 },
    uPan: { value: new THREE.Vector2() },
  };

  const materialOptions = {
    depthTest: false,
    depthWrite: false,
  };

  const sceneMaterial = new THREE.ShaderMaterial({
    ...materialOptions,
    ...(renderer.capabilities.isWebGL2
      ? {}
      : { extensions: { derivatives: true } }),
    uniforms: sceneUniforms,
    vertexShader: sceneVertex,
    fragmentShader: sceneFragment,
  });

  const finalMaterial = new THREE.ShaderMaterial({
    ...materialOptions,
    uniforms: finalUniforms,
    vertexShader: sceneVertex,
    fragmentShader: finalFragment,
  });

  let sceneTarget = null;
  let sceneWidth = 0;
  let sceneHeight = 0;
  let quality = 1.0;
  let lastQualityCheck = 0;
  let frameTimeSum = 0;
  let frameSamples = 0;
  let lastFrame = 0;
  let elapsed = 0;
  let pointerX = 0;
  let pointerY = 0;
  let targetPointerX = 0;
  let targetPointerY = 0;
  let resizeFrame = 0;
  let contextLost = false;
  const motionPreference = window.matchMedia(
    "(prefers-reduced-motion: reduce)",
  );
  let reduceMotion = motionPreference.matches;

  const cameraPosition = new THREE.Vector3();
  const cameraBasis = new THREE.Matrix3();
  const forward = new THREE.Vector3();
  const right = new THREE.Vector3();
  const up = new THREE.Vector3();
  const worldUp = new THREE.Vector3(0, 1, 0);
  const drawingBuffer = new THREE.Vector2();

  function makeTarget(width, height) {
    const target = new THREE.WebGLRenderTarget(
      Math.max(2, Math.floor(width)),
      Math.max(2, Math.floor(height)),
      {
        type: renderTargetType,
        format: THREE.RGBAFormat,
        minFilter: THREE.LinearFilter,
        magFilter: THREE.LinearFilter,
        depthBuffer: false,
        stencilBuffer: false,
        generateMipmaps: false,
      },
    );
    target.texture.wrapS = THREE.ClampToEdgeWrapping;
    target.texture.wrapT = THREE.ClampToEdgeWrapping;
    return target;
  }

  function disposeTarget(target) {
    if (target) {
      target.dispose();
    }
  }

  function buildTargets() {
    disposeTarget(sceneTarget);
    sceneTarget = makeTarget(sceneWidth, sceneHeight);
  }

  function updateViewport() {
    const pixelRatio = Math.min(window.devicePixelRatio || 1, 1.5);
    renderer.setPixelRatio(pixelRatio);
    renderer.setSize(window.innerWidth, window.innerHeight, false);
    renderer.getDrawingBufferSize(drawingBuffer);

    const budgetScale = Math.sqrt(
      4500000 / Math.max(1, drawingBuffer.x * drawingBuffer.y),
    );
    const scale = Math.max(
      0.25,
      Math.min(quality, budgetScale),
    );
    const nextWidth = Math.max(
      2,
      Math.round(drawingBuffer.x * scale),
    );
    const nextHeight = Math.max(
      2,
      Math.round(drawingBuffer.y * scale),
    );

    if (
      nextWidth !== sceneWidth
      || nextHeight !== sceneHeight
      || !sceneTarget
    ) {
      sceneWidth = nextWidth;
      sceneHeight = nextHeight;
      buildTargets();
    }

    sceneUniforms.uResolution.value.set(sceneWidth, sceneHeight);
    finalUniforms.uSceneTexelSize.value.set(
      1 / sceneWidth,
      1 / sceneHeight,
    );
  }

  function updateCamera(delta) {
    if (!reduceMotion) {
      pointerX += (targetPointerX - pointerX) * Math.min(1, delta * 2.4);
      pointerY += (targetPointerY - pointerY) * Math.min(1, delta * 2.4);
    }

    const azimuth = reduceMotion
      ? 0
      : pointerX * 0.018;
    const inclination = 0.30
      + (reduceMotion
        ? 0
        : pointerY * 0.008);
    const distance = 17.0;
    const horizontal = Math.cos(inclination) * distance;

    cameraPosition.set(
      horizontal * Math.sin(azimuth),
      Math.sin(inclination) * distance,
      horizontal * Math.cos(azimuth),
    );

    forward.copy(cameraPosition).multiplyScalar(-1.0).normalize();
    right.crossVectors(forward, worldUp).normalize();
    up.crossVectors(right, forward).normalize();

    cameraBasis.set(
      right.x, up.x, forward.x,
      right.y, up.y, forward.y,
      right.z, up.z, forward.z,
    );

    sceneUniforms.uCamPos.value.copy(cameraPosition);
    sceneUniforms.uCamBasis.value.copy(cameraBasis);
    sceneUniforms.uTime.value = elapsed;
    sceneUniforms.uPan.value.set(
      reduceMotion ? 0 : pointerX * 0.028,
      reduceMotion ? 0 : -pointerY * 0.014,
    );
    finalUniforms.uPan.value.copy(sceneUniforms.uPan.value);
  }

  function renderPass(material, target) {
    quad.material = material;
    renderer.setRenderTarget(target);
    renderer.clear(true, false, false);
    renderer.render(quadScene, quadCamera);
  }

  function renderFrame(now) {
    if (contextLost) {
      return;
    }

    if (!lastFrame) {
      lastFrame = now;
    }

    const rawDelta = now - lastFrame;
    const delta = Math.min(rawDelta / 1000, 0.05);
    lastFrame = now;
    elapsed += delta;
    frameTimeSum += rawDelta;
    frameSamples++;

    updateCamera(delta);

    renderPass(sceneMaterial, sceneTarget);

    finalUniforms.uScene.value = sceneTarget.texture;
    renderPass(finalMaterial, null);

    if (
      !reduceMotion
      && frameSamples >= 24
      && now - lastQualityCheck > 1200
    ) {
      const averageFrame = frameTimeSum / frameSamples;
      let nextQuality = quality;

      if (averageFrame > 29) {
        nextQuality = Math.max(0.78, quality - 0.055);
      } else if (averageFrame < 15.5) {
        nextQuality = Math.min(1.0, quality + 0.030);
      }

      if (Math.abs(nextQuality - quality) > 0.015) {
        quality = nextQuality;
        sceneUniforms.uSteps.value = quality < 0.9 ? 150 : 170;
        updateViewport();
      }

      frameTimeSum = 0;
      frameSamples = 0;
      lastQualityCheck = now;
    }

    if (!document.hidden && !reduceMotion) {
      window.requestAnimationFrame(renderFrame);
    }
  }

  function resize() {
    updateViewport();
    if (document.hidden) {
      return;
    }
    lastFrame = 0;
    if (reduceMotion) {
      window.requestAnimationFrame(renderFrame);
    }
  }

  window.addEventListener("resize", () => {
    if (resizeFrame) {
      return;
    }
    resizeFrame = window.requestAnimationFrame(() => {
      resizeFrame = 0;
      resize();
    });
  });

  window.addEventListener("pointermove", (event) => {
    if (reduceMotion) {
      return;
    }
    targetPointerX = (event.clientX / Math.max(1, window.innerWidth)) * 2 - 1;
    targetPointerY = (event.clientY / Math.max(1, window.innerHeight)) * 2 - 1;
  }, { passive: true });

  motionPreference.addEventListener("change", (event) => {
    reduceMotion = event.matches;
    lastFrame = 0;
    if (!reduceMotion && !document.hidden) {
      window.requestAnimationFrame(renderFrame);
    }
  });

  document.addEventListener("visibilitychange", () => {
    if (!document.hidden && !contextLost && !reduceMotion) {
      lastFrame = 0;
      window.requestAnimationFrame(renderFrame);
    }
  });

  canvas.addEventListener("webglcontextlost", (event) => {
    event.preventDefault();
    contextLost = true;
    showFallback();
  });

  updateViewport();
  updateCamera(0);
  window.requestAnimationFrame(renderFrame);
}());
