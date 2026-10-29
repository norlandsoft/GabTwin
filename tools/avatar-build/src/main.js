// GabTwin 的数字人渲染层（M3.0 占位角色）。
//
// 这里刻意只依赖 three：先用几何体搭一个能眨眼、呼吸、说话的角色，
// 把「音频时钟 → 口型 → 状态机」跑通。M3.1 再把 asset 层换成 VRM。
//
// 输入只有两种：
//   setLevel(rms)   输出回调实测的 30 ms RMS（0..1 左右）
//   setState(name)  idle / listening / thinking / speaking / interrupted / error
// 音频数据本身不经过 IPC。

import * as THREE from "three";

const TAU = Math.PI * 2;
const clamp = (v, lo, hi) => Math.min(hi, Math.max(lo, v));
const damp = (current, target, lambda, dt) =>
  current + (target - current) * (1 - Math.exp(-lambda * dt));
const random = (min, max) => min + Math.random() * (max - min);

function material(color, extra = {}) {
  return new THREE.MeshStandardMaterial({
    color,
    roughness: 0.62,
    metalness: 0.02,
    ...extra,
  });
}

function addMesh(parent, geometry, mat, position, scale, rotation) {
  const mesh = new THREE.Mesh(geometry, mat);
  if (position) mesh.position.set(...position);
  if (scale) mesh.scale.set(...scale);
  if (rotation) mesh.rotation.set(...rotation);
  parent.add(mesh);
  return mesh;
}

export class Avatar {
  constructor(canvas) {
    this.canvas = canvas;
    this.state = "idle";
    this.emotion = "neutral";
    this.speaking = false;
    this.rawLevel = 0;
    this.level = 0;
    this.micLevel = 0;
    this.mouth = 0.015;
    this.blink = 0;
    this.nextBlink = random(2.0, 4.0);
    this.time = 0;
    this.lastTime = performance.now() / 1000;
    this.lastRender = 0;
    this.gazeX = 0;
    this.gazeY = 0;
    this.gazeTargetX = 0;
    this.gazeTargetY = 0;
    this.nextSaccade = random(1.0, 3.0);
    this.nod = 0;
    this.prevLevel = 0;
    this.interruptedAt = -10;
    this.resumeState = "idle";
    this.visible = true;

    this.renderer = new THREE.WebGLRenderer({
      canvas,
      antialias: true,
      alpha: true,
      powerPreference: "high-performance",
    });
    this.renderer.setPixelRatio(Math.min(window.devicePixelRatio || 1, 2));
    this.renderer.outputColorSpace = THREE.SRGBColorSpace;
    this.renderer.toneMapping = THREE.ACESFilmicToneMapping;
    this.renderer.toneMappingExposure = 1.12;

    this.scene = new THREE.Scene();
    this.camera = new THREE.PerspectiveCamera(30, 1, 0.1, 100);
    this.camera.position.set(0, 0.16, 6.7);
    this.camera.lookAt(0, 0.02, 0);

    this.root = new THREE.Group();
    this.head = new THREE.Group();
    this.head.position.set(0, 0.16, 0);
    this.scene.add(this.root);
    this.root.add(this.head);

    this.#buildLights();
    this.#buildBody();
    this.#buildHead();

    this.resizeObserver = new ResizeObserver(() => this.resize());
    this.resizeObserver.observe(canvas.parentElement || canvas);
    this.resize();

    this.#bindContext();
    this.lastTime = performance.now() / 1000;
    this.loop = this.loop.bind(this);
    requestAnimationFrame(this.loop);
  }

  #buildLights() {
    this.scene.add(new THREE.HemisphereLight(0xfff1e4, 0x241a22, 1.35));

    const key = new THREE.DirectionalLight(0xffe1c8, 2.6);
    key.position.set(2.6, 3.2, 4.0);
    this.scene.add(key);

    const fill = new THREE.DirectionalLight(0xffc3d2, 0.95);
    fill.position.set(-3.2, 1.0, 2.4);
    this.scene.add(fill);

    const rim = new THREE.DirectionalLight(0x9fc3ff, 1.15);
    rim.position.set(-1.2, 2.2, -4.0);
    this.scene.add(rim);

    const warm = new THREE.PointLight(0xffd6ae, 0.7, 12, 2);
    warm.position.set(0.6, 1.2, 3.0);
    this.scene.add(warm);
  }

  #buildBody() {
    const skin = material(0xf2c9b7, { roughness: 0.78 });
    const sweater = material(0xe8b4ba, { roughness: 0.86 });
    const collar = material(0xf6d8d8, { roughness: 0.8 });

    this.body = new THREE.Group();
    this.root.add(this.body);

    addMesh(
      this.body,
      new THREE.CylinderGeometry(0.22, 0.26, 0.55, 24),
      skin,
      [0, -1.05, 0]
    );

    const shoulders = addMesh(
      this.body,
      new THREE.SphereGeometry(0.98, 32, 24),
      sweater,
      [0, -1.74, -0.06],
      [1.18, 0.62, 0.72]
    );
    shoulders.rotation.x = 0.04;

    addMesh(
      this.body,
      new THREE.TorusGeometry(0.265, 0.055, 12, 36),
      collar,
      [0, -1.13, 0.04],
      [1, 1, 1],
      [Math.PI / 2, 0, 0]
    );

    // 一条细项链，给胸口一点反光。
    addMesh(
      this.body,
      new THREE.TorusGeometry(0.24, 0.012, 8, 32, Math.PI * 0.9),
      material(0xe3b26a, { metalness: 0.55, roughness: 0.28 }),
      [0, -1.2, 0.3],
      [1, 1, 1],
      [Math.PI / 2, 0, 0]
    );
  }

  #buildHead() {
    this.skin = material(0xf3cdbb, { roughness: 0.76 });
    this.hair = material(0x3a292b, { roughness: 0.84 });
    this.hairLight = material(0x563b3a, { roughness: 0.82 });
    this.dark = material(0x2a1a1a, { roughness: 0.5 });
    this.eyeWhite = material(0xfffaf6, { roughness: 0.25 });
    this.iris = material(0x7a4a3a, { roughness: 0.22 });
    this.pupil = material(0x1b1010, { roughness: 0.2 });
    this.lip = material(0xb9636b, { roughness: 0.5 });
    this.mouthDark = material(0x4a1f24, { roughness: 0.55 });
    this.tongue = material(0xd97b85, { roughness: 0.6 });
    this.blush = new THREE.MeshBasicMaterial({
      color: 0xff8f9a,
      transparent: true,
      opacity: 0.2,
      depthWrite: false,
    });
    this.gold = material(0xe3b26a, { metalness: 0.55, roughness: 0.3 });

    // 头骨。脸朝 +z。
    addMesh(
      this.head,
      new THREE.SphereGeometry(1, 48, 36),
      this.skin,
      [0, 0, 0],
      [0.9, 1.02, 0.86]
    );

    // 耳朵（大部分会被头发盖住）。
    for (const side of [-1, 1]) {
      addMesh(
        this.head,
        new THREE.SphereGeometry(0.15, 16, 12),
        this.skin,
        [side * 0.86, 0.02, 0.02],
        [0.55, 0.8, 0.4]
      );
    }

    this.#buildHair();
    this.#buildEyes();
    this.#buildBrows();
    this.#buildNose();
    this.#buildMouth();
    this.#buildBlush();
  }

  #buildHair() {
    // 头顶：一段球面，前沿停在额头以上，避免盖住眉眼。
    addMesh(
      this.head,
      new THREE.SphereGeometry(1.05, 48, 32, 0, TAU, 0, Math.PI * 0.34),
      this.hair,
      [0, 0.02, -0.02],
      [0.93, 1.0, 0.9]
    );

    // 后脑勺的头发体积。
    addMesh(
      this.head,
      new THREE.SphereGeometry(1.0, 32, 24),
      this.hair,
      [0, 0.02, -0.3],
      [0.95, 1.02, 0.7]
    );

    // 刘海：压扁的球叠成一片，中间短、两侧长，盖住发际线但不遮眉。
    const bangs = [
      [-0.58, 0.52, 0.56, 0.62, 1.15, 0.36, 0.18],
      [-0.32, 0.6, 0.64, 0.8, 0.98, 0.4, 0.08],
      [-0.04, 0.63, 0.67, 0.84, 0.9, 0.42, 0.02],
      [0.26, 0.61, 0.65, 0.8, 0.95, 0.4, -0.05],
      [0.54, 0.53, 0.57, 0.64, 1.12, 0.37, -0.16],
    ];
    for (const [x, y, z, sx, sy, sz, rz] of bangs) {
      const lobe = addMesh(
        this.head,
        new THREE.SphereGeometry(0.3, 16, 12),
        this.hair,
        [x, y, z],
        [sx, sy, sz],
        [-0.28, 0, rz]
      );
      lobe.rotation.x = -0.28;
    }

    // 两侧长发。
    for (const side of [-1, 1]) {
      addMesh(
        this.head,
        new THREE.CapsuleGeometry(0.105, 0.72, 6, 12),
        this.hair,
        [side * 0.82, -0.28, 0.28],
        [1, 1, 0.8],
        [0.05, 0, side * 0.08]
      );
    }

    // 双马尾。
    this.tails = [];
    for (const side of [-1, 1]) {
      const tail = addMesh(
        this.head,
        new THREE.CapsuleGeometry(0.145, 0.82, 6, 12),
        this.hairLight,
        [side * 0.98, -0.55, -0.3],
        [1, 1, 0.85],
        [-0.25, 0, side * 0.3]
      );
      this.tails.push(tail);
      addMesh(
        this.head,
        new THREE.TorusGeometry(0.115, 0.032, 8, 16),
        this.gold,
        [side * 0.86, -0.15, -0.18],
        [1, 1, 1],
        [0.4, 0, side * 0.3]
      );
    }

    // 一侧的小发饰。
    const pin = addMesh(
      this.head,
      new THREE.OctahedronGeometry(0.075, 0),
      this.gold,
      [0.63, 0.55, 0.5],
      [1, 1.4, 0.5],
      [0.2, 0, 0.4]
    );
    pin.rotation.y = 0.3;
  }

  #buildEyes() {
    this.eyes = [];
    this.irises = [];

    for (const side of [-1, 1]) {
      const eye = new THREE.Group();
      eye.position.set(side * 0.315, 0.08, 0.8);
      this.head.add(eye);

      const white = addMesh(
        eye,
        new THREE.SphereGeometry(0.17, 24, 18),
        this.eyeWhite,
        [0, 0, 0],
        [1, 1.22, 0.34]
      );
      white.renderOrder = 1;

      const iris = new THREE.Group();
      iris.position.set(0, 0, 0.062);
      eye.add(iris);
      this.irises.push(iris);

      addMesh(
        iris,
        new THREE.CircleGeometry(0.098, 28),
        this.iris,
        [0, 0, 0]
      );
      addMesh(
        iris,
        new THREE.CircleGeometry(0.048, 20),
        this.pupil,
        [0, 0, 0.002]
      );
      addMesh(
        iris,
        new THREE.CircleGeometry(0.026, 16),
        new THREE.MeshBasicMaterial({ color: 0xffffff }),
        [-0.03, 0.035, 0.004]
      );
      addMesh(
        iris,
        new THREE.CircleGeometry(0.012, 12),
        new THREE.MeshBasicMaterial({ color: 0xffffff }),
        [0.032, -0.026, 0.004]
      );

      // 上睫毛。
      addMesh(
        eye,
        new THREE.BoxGeometry(0.31, 0.032, 0.035),
        this.dark,
        [0, 0.185, 0.035],
        [1, 1, 1],
        [0, 0, side * 0.12]
      );

      this.eyes.push(eye);
    }
  }

  #buildBrows() {
    this.brows = [];
    for (const side of [-1, 1]) {
      const brow = addMesh(
        this.head,
        new THREE.BoxGeometry(0.235, 0.026, 0.03),
        this.hair,
        [side * 0.315, 0.37, 0.8],
        [1, 1, 1],
        [0, 0, -side * 0.1]
      );
      this.brows.push(brow);
    }
  }

  #buildNose() {
    addMesh(
      this.head,
      new THREE.SphereGeometry(0.05, 16, 12),
      material(0xe8b8a6, { roughness: 0.8 }),
      [0, -0.07, 0.86],
      [0.8, 0.65, 0.55]
    );
  }

  #buildMouth() {
    this.mouthGroup = new THREE.Group();
    this.mouthGroup.position.set(0, -0.335, 0.805);
    this.head.add(this.mouthGroup);

    this.opening = addMesh(
      this.mouthGroup,
      new THREE.CircleGeometry(0.108, 28),
      this.mouthDark,
      [0, 0, 0],
      [1, 0.13, 1]
    );

    this.tongueMesh = addMesh(
      this.mouthGroup,
      new THREE.CircleGeometry(0.07, 20),
      this.tongue,
      [0, -0.025, 0.003],
      [1, 0.45, 1]
    );

    this.smile = addMesh(
      this.mouthGroup,
      new THREE.TorusGeometry(0.1, 0.012, 8, 24, Math.PI),
      this.lip,
      [0, 0.02, 0.006],
      [1, 1, 1],
      [0, 0, Math.PI]
    );

    this.upperLip = addMesh(
      this.mouthGroup,
      new THREE.BoxGeometry(0.23, 0.022, 0.02),
      this.lip,
      [0, 0.085, 0.004],
      [1, 1, 1]
    );
  }

  #buildBlush() {
    for (const side of [-1, 1]) {
      const blush = addMesh(
        this.head,
        new THREE.CircleGeometry(0.13, 24),
        this.blush,
        [side * 0.5, -0.15, 0.78],
        [1, 0.62, 1],
        [0, side * 0.28, 0]
      );
      blush.renderOrder = 2;
    }
  }

  #bindContext() {
    this.canvas.addEventListener("webglcontextlost", (event) => {
      event.preventDefault();
      this.visible = false;
    });
    this.canvas.addEventListener("webglcontextrestored", () => {
      this.visible = true;
    });
  }

  resize() {
    const parent = this.canvas.parentElement;
    const width = Math.max(1, parent ? parent.clientWidth : this.canvas.clientWidth);
    const height = Math.max(1, parent ? parent.clientHeight : this.canvas.clientHeight);
    this.renderer.setSize(width, height, false);
    this.camera.aspect = width / height;
    this.camera.updateProjectionMatrix();
  }

  setState(state) {
    if (!state) return;
    if (state === "interrupted") {
      this.interrupt();
      return;
    }
    if (this.state === "interrupted" && state !== "interrupted") {
      this.interruptedAt = -10;
    }
    this.state = state;
    this.speaking = state === "speaking";

    if (state === "thinking") {
      this.gazeTargetX = -0.028;
      this.gazeTargetY = 0.032;
      this.nextSaccade = 2.5;
    } else if (state === "speaking" || state === "idle" || state === "error") {
      this.gazeTargetX = 0;
      this.gazeTargetY = 0;
    } else if (state === "listening") {
      this.gazeTargetX = random(-0.012, 0.012);
      this.gazeTargetY = random(-0.006, 0.006);
      this.nextSaccade = random(1.2, 2.4);
    }
    if (state !== "speaking") {
      this.rawLevel = 0;
    }
  }

  setLevel(rms) {
    const value = Number.isFinite(rms) ? clamp(rms, 0, 1) : 0;
    this.rawLevel = value;
    if (this.state === "speaking" && value > 0.045 && this.prevLevel <= 0.045) {
      this.nod = Math.min(1.2, this.nod + 0.65);
    }
    this.prevLevel = value;
  }

  setMicLevel(rms) {
    this.micLevel = Number.isFinite(rms) ? clamp(rms * 6, 0, 1) : 0;
  }

  setPlayedMs(_ms) {
    // M3.1 的口型时间轴会用到；M3.0 只靠 RMS。
  }

  setEmotion(emotion) {
    this.emotion = emotion || "neutral";
  }

  interrupt() {
    this.resumeState =
      this.state === "listening" || this.state === "thinking" ? this.state : "idle";
    this.state = "interrupted";
    this.speaking = false;
    this.rawLevel = 0;
    this.level = 0;
    this.mouth = Math.min(this.mouth, 0.08);
    this.gazeTargetX = 0;
    this.gazeTargetY = 0;
    this.interruptedAt = this.time;
  }

  isSpeaking() {
    return this.speaking;
  }

  loop() {
    requestAnimationFrame(this.loop);
    if (!this.visible) return;
    const now = performance.now() / 1000;
    // 30 fps 上限：口型够用，也给 TTS 留 GPU。
    if (now - this.lastRender < 1 / 30) return;
    this.lastRender = now;
    const dt = Math.min(0.05, Math.max(0.001, now - this.lastTime));
    this.lastTime = now;
    this.update(dt);
    this.renderer.render(this.scene, this.camera);
  }

  update(dt) {
    this.time += dt;
    const t = this.time;

    if (this.state === "interrupted" && t - this.interruptedAt > 0.38) {
      this.state = this.resumeState || "idle";
      this.speaking = false;
    }

    const breath = Math.sin(t * 1.55);
    const state = this.state;

    let headRX = 0.015;
    let headRY = Math.sin(t * 0.34) * 0.035;
    let headRZ = Math.sin(t * 0.27) * 0.008;
    let eyeOpen = 1.0;
    let browLift = 0;
    let browTilt = 0;
    let mouthTarget = 0.012;
    let gazeX = Math.sin(t * 0.3) * 0.008;
    let gazeY = Math.sin(t * 0.4) * 0.004;

    if (state === "listening") {
      headRX = 0.035 + this.micLevel * 0.03;
      headRY = Math.sin(t * 0.5) * 0.018;
      headRZ = 0.028;
      eyeOpen = 1.06;
      browLift = 0.014;
      mouthTarget = 0.02;
      gazeX = 0;
      gazeY = 0;
    } else if (state === "thinking") {
      headRX = -0.06;
      headRY = 0.1 + Math.sin(t * 0.6) * 0.015;
      headRZ = 0.04;
      eyeOpen = 0.9;
      browLift = -0.008;
      browTilt = 0.08;
      mouthTarget = 0.008;
      gazeX = -0.03;
      gazeY = 0.035;
    } else if (state === "speaking") {
      headRX = 0.01 + this.nod * 0.045;
      headRY = Math.sin(t * 1.7) * 0.02;
      headRZ = Math.sin(t * 0.9) * 0.012;
      eyeOpen = 1.0;
      browLift = 0.006;
      mouthTarget = 0.015 + this.level * 0.98;
      gazeX = Math.sin(t * 1.1) * 0.01;
      gazeY = Math.sin(t * 1.4) * 0.006;
    } else if (state === "interrupted") {
      headRX = -0.045;
      headRY = 0;
      headRZ = 0;
      eyeOpen = 1.18;
      browLift = 0.03;
      mouthTarget = 0;
      gazeX = 0;
      gazeY = 0;
    } else if (state === "error") {
      headRX = 0.02;
      headRY = Math.sin(t * 4.0) * 0.02;
      headRZ = 0;
      eyeOpen = 0.88;
      browLift = -0.018;
      browTilt = -0.12;
      mouthTarget = 0.005;
      gazeX = 0;
      gazeY = -0.01;
    } else {
      // idle
      headRX = 0.015 + breath * 0.012;
      headRY = Math.sin(t * 0.34) * 0.045;
      headRZ = Math.sin(t * 0.27) * 0.012;
      eyeOpen = 1.0;
      browLift = 0.002;
      mouthTarget = 0.014 + breath * 0.004;
      gazeX = Math.sin(t * 0.3) * 0.012;
      gazeY = Math.sin(t * 0.4) * 0.006;
    }

    // 说话时嘴的开合：attack 快、release 慢，避免"抖嘴"。
    if (state === "speaking") {
      const target = this.rawLevel < 0.008 ? 0 : clamp(Math.pow(this.rawLevel * 6.4, 0.72), 0, 1);
      this.level = damp(this.level, target, target > this.level ? 28 : 12, dt);
      mouthTarget = 0.015 + this.level * 0.98;
    } else {
      this.level = damp(this.level, 0, 18, dt);
    }
    this.mouth = damp(this.mouth, mouthTarget, 30, dt);

    // 头部姿态。
    this.head.rotation.x = damp(this.head.rotation.x, headRX, 6, dt);
    this.head.rotation.y = damp(this.head.rotation.y, headRY, 5, dt);
    this.head.rotation.z = damp(this.head.rotation.z, headRZ, 5, dt);
    this.head.position.y = 0.16 + breath * 0.01;
    this.root.rotation.z = Math.sin(t * 0.45) * 0.006;
    this.root.position.x = Math.sin(t * 0.3) * 0.008;

    // 呼吸：肩膀轻微起伏。
    this.body.scale.y = 1 + breath * 0.006;

    // 眨眼。
    this.nextBlink -= dt;
    if (this.nextBlink <= 0) {
      this.blink = 1;
      this.nextBlink = random(2.4, 4.8);
      if (Math.random() < 0.18) this.nextBlink = random(0.16, 0.22);
    }
    this.blink = Math.max(0, this.blink - dt / 0.13);
    const blinkScale = 1 - this.blink * 0.94;
    const eyeScale = Math.max(0.06, eyeOpen * blinkScale);
    for (const eye of this.eyes) {
      eye.scale.y = damp(eye.scale.y, eyeScale, 28, dt);
    }

    // 眼珠扫视。
    this.nextSaccade -= dt;
    if (this.nextSaccade <= 0) {
      this.gazeTargetX = random(-0.018, 0.018);
      this.gazeTargetY = random(-0.012, 0.012);
      this.nextSaccade = random(1.4, 3.2);
    }
    if (state === "thinking") {
      this.gazeTargetX = -0.03;
      this.gazeTargetY = 0.035;
    } else if (state === "listening" || state === "speaking") {
      this.gazeTargetX = damp(this.gazeTargetX, 0, 3, dt);
      this.gazeTargetY = damp(this.gazeTargetY, 0, 3, dt);
    }
    this.gazeX = damp(this.gazeX, clamp(gazeX + this.gazeTargetX, -0.05, 0.05), 8, dt);
    this.gazeY = damp(this.gazeY, clamp(gazeY + this.gazeTargetY, -0.04, 0.04), 8, dt);
    for (const iris of this.irises) {
      iris.position.x = this.gazeX;
      iris.position.y = this.gazeY;
    }

    // 眉毛。
    this.brows[0].position.y = 0.37 + browLift;
    this.brows[1].position.y = 0.37 + browLift;
    this.brows[0].rotation.z = -0.1 - browTilt;
    this.brows[1].rotation.z = 0.1 + browTilt;

    // 嘴形：M3.0 只有开合；圆唇/扁唇留给 viseme。
    this.opening.scale.y = 0.13 + this.mouth * 1.18;
    this.opening.scale.x = 1.0 + this.level * 0.05;
    this.opening.position.y = -this.mouth * 0.035;
    const open = this.mouth > 0.05;
    this.opening.visible = open;
    this.tongueMesh.visible = open && this.mouth > 0.12;
    this.smile.visible = !open;
    this.upperLip.visible = !open;

    // 说话时眉毛与句尾点头。
    this.nod = Math.max(0, this.nod - dt * 3.2);
    this.micLevel = damp(this.micLevel, 0, 6, dt);
  }
}

export function init(options = {}) {
  const canvas = options.canvas;
  if (!canvas) return null;
  try {
    return new Avatar(canvas);
  } catch (error) {
    console.error("[avatar] 初始化失败", error);
    canvas.style.display = "none";
    return null;
  }
}
