// Moni app UI. Talks to the Rust side through Tauri commands; the service
// itself never talks to this window, it just writes status.json.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
let config = null;
let portsKey = "";
let fansKey = "";
let tempsKey = "";
let flashing = false;

// ---------- settings ----------

function readForm() {
  const transport = document.querySelector('input[name="transport"]:checked')?.value ?? "auto";
  const wifiMode = document.querySelector('input[name="wifimode"]:checked')?.value ?? "auto";
  return {
    ...config,
    enabled: $("enabled").checked,
    transport,
    wifi_target: wifiMode === "fixed" ? $("wifi-target").value.trim() : "",
    serial_port: $("serial-port").value,
    fan_sensor: $("fan-sensor").value,
    mobo_sensor: $("mobo-sensor").value,
    ssd_sensor: $("ssd-sensor").value,
    interval_ms: Number($("interval").value),
    control_key: $("control-key").value.trim(),
    lights: readLights(),
    display: readDisplay(),
    fans: readFans(),
  };
}

// ---------- fan control ----------

const FAN_ROLES = [
  ["cpu", "CPU curve"],
  ["intake", "Intake"],
  ["exhaust", "Exhaust"],
  ["pump", "Pump (100 %)"],
  ["bios", "Leave to the BIOS"],
];
let fanGroups = [];
let fanControls = []; // the board's fan control sensors (LibreHardwareMonitor "Control" rows)
let fanControlsKey = "";
let fanSpeeds = {}; // control id -> % Moni sets now

function readFans() {
  const point = (id, i, k) => Number($(`${id}-${i}-${k}`)?.value);
  const curve = (id, old) =>
    old.map((p, i) => [point(id, i, "t"), point(id, i, "p")]).map(([t, p], i) => (Number.isFinite(t) && Number.isFinite(p) ? [t, p] : old[i]));
  return {
    ...config.fans,
    enabled: $("fans-enabled").checked,
    groups: fanGroups.map((g) => ({ ...g })),
    cpu_curve: curve("curve-cpu", config.fans.cpu_curve),
    gpu_curve: curve("curve-gpu", config.fans.gpu_curve),
    intake_offset: Number($("fans-offset").value),
    hysteresis: Number($("fans-hysteresis").value),
  };
}

function fillFans() {
  const f = config.fans;
  $("fans-enabled").checked = f.enabled;
  fanGroups = f.groups.map((g) => ({ ...g }));
  $("fans-offset").value = String(f.intake_offset);
  $("fans-hysteresis").value = String(f.hysteresis);
  fillCurve("curve-cpu", f.cpu_curve);
  fillCurve("curve-gpu", f.gpu_curve);
  renderFanGroups();
  showFanValues();
}

function fillCurve(id, points) {
  $(id).replaceChildren(
    ...points.map(([t, p], i) => {
      const label = document.createElement("label");
      const temp = Object.assign(document.createElement("input"), { type: "text", id: `${id}-${i}-t`, value: String(t), inputMode: "decimal" });
      const pct = Object.assign(document.createElement("input"), { type: "text", id: `${id}-${i}-p`, value: String(p), inputMode: "decimal" });
      for (const input of [temp, pct]) input.addEventListener("change", save);
      label.append(temp, "°C →", pct, "%");
      return label;
    }),
  );
}

function showFanValues() {
  $("fans-offset-value").textContent = `${$("fans-offset").value} %`;
  $("fans-hysteresis-value").textContent = `${$("fans-hysteresis").value} °C`;
}

function controlLabel(row) {
  return `${row.name} · ${row.hw} · ${fmt(row.value)} %`;
}

function renderFanGroups() {
  $("fan-groups").replaceChildren(
    ...fanGroups.map((g, i) => {
      const box = document.createElement("div");
      box.className = "fan-group";
      const name = Object.assign(document.createElement("input"), { type: "text", value: g.name, className: "fan-name", spellcheck: false });
      name.setAttribute("aria-label", "Group name");
      name.addEventListener("change", () => ((fanGroups[i].name = name.value.trim()), save()));
      const speed = document.createElement("span");
      speed.className = "fan-speed";
      speed.id = `fan-speed-${i}`;
      const line = document.createElement("div");
      line.className = "fan-line";
      const role = document.createElement("select");
      role.setAttribute("aria-label", "What it follows");
      role.replaceChildren(...FAN_ROLES.map(([v, label]) => new Option(label, v)));
      role.value = g.role;
      role.addEventListener("change", () => ((fanGroups[i].role = role.value), save(), renderFanGroups()));
      const header = document.createElement("select");
      header.className = "fan-header";
      header.dataset.group = String(i);
      header.setAttribute("aria-label", "Fan header");
      fillHeaderOptions(header, g.control);
      header.addEventListener("change", () => ((fanGroups[i].control = header.value), save()));
      const min = Object.assign(document.createElement("input"), { type: "text", value: String(g.min), className: "fan-min", inputMode: "numeric", title: "Minimum %" });
      min.setAttribute("aria-label", "Minimum speed, %");
      min.disabled = g.role === "pump";
      min.addEventListener("change", () => {
        const n = Number(min.value);
        if (Number.isInteger(n) && n >= 20 && n <= 100) fanGroups[i].min = n;
        min.value = String(fanGroups[i].min);
        save();
      });
      const identify = Object.assign(document.createElement("button"), { textContent: "Identify", title: "Spin this header to 100 % for 6 s" });
      identify.disabled = !g.control;
      identify.addEventListener("click", async () => {
        identify.disabled = true;
        try {
          await invoke("identify_fan", { control: fanGroups[i].control });
        } catch (error) {
          $("settings-error").textContent = String(error);
          $("settings-error").hidden = false;
        }
        setTimeout(() => (identify.disabled = !fanGroups[i].control), 6000);
      });
      line.append(role, header, "min", min, "%", identify);
      box.append(name, speed, line);
      return box;
    }),
  );
  showFanSpeeds();
}

function fillHeaderOptions(select, value) {
  const options = [["", "No header yet"]].concat(fanControls.map((r) => [r.id, controlLabel(r)]));
  setOptions(select, options, value);
}

function updateFanControls(rows) {
  fanControls = rows.filter((r) => r.type.toLowerCase() === "control" && !r.id.toLowerCase().includes("/gpu"));
  const key = JSON.stringify(fanControls.map((r) => [r.id, Math.round(r.value)]));
  if (key === fanControlsKey) return;
  fanControlsKey = key;
  for (const select of document.querySelectorAll(".fan-header")) {
    if (document.activeElement === select) continue;
    fillHeaderOptions(select, fanGroups[Number(select.dataset.group)]?.control ?? "");
  }
}

function showFanSpeeds() {
  fanGroups.forEach((g, i) => {
    const node = $(`fan-speed-${i}`);
    if (!node) return;
    const pct = g.control ? fanSpeeds[g.control] : undefined;
    node.textContent = !g.control ? "no header" : pct == null ? "BIOS" : `${Math.round(pct)} %`;
  });
}

function renderFans(status) {
  fanSpeeds = Object.fromEntries((status.fans ?? []).filter((f) => f.speed != null).map((f) => [f.control, f.speed]));
  showFanSpeeds();
  const note = status.fans_note || "";
  if (!config?.fans.enabled) pill("pill-fans", "Off");
  else if (note.startsWith("on:")) pill("pill-fans", "On", "good");
  else if (note.startsWith("too hot")) pill("pill-fans", "Full speed", "bad");
  else pill("pill-fans", note.startsWith("paused") ? "Paused" : "Waiting", "warn");
  $("fans-note").textContent = config?.fans.enabled ? note : "";
}

// ---------- digital display ----------

function readDisplay() {
  const cycle = Number($("display-cycle").value.trim());
  const items = [...document.querySelectorAll(".display-item:checked")].reduce((sum, box) => sum + Number(box.value), 0);
  return {
    ...config.display,
    enabled: $("display-enabled").checked,
    fahrenheit: document.querySelector('input[name="display-unit"]:checked')?.value === "f",
    mode: Number($("display-mode").value),
    show_gpu: document.querySelector('input[name="display-source"]:checked')?.value === "gpu",
    cycle: Number.isInteger(cycle) && cycle >= 1 && cycle <= 60 ? cycle : config.display.cycle,
    items: items || 1,
  };
}

function fillDisplay() {
  const d = config.display;
  $("display-enabled").checked = d.enabled;
  for (const radio of document.querySelectorAll('input[name="display-unit"]')) radio.checked = radio.value === (d.fahrenheit ? "f" : "c");
  $("display-mode").value = String(d.mode);
  for (const radio of document.querySelectorAll('input[name="display-source"]')) radio.checked = radio.value === (d.show_gpu ? "gpu" : "cpu");
  for (const box of document.querySelectorAll(".display-item")) box.checked = (d.items & Number(box.value)) !== 0;
  $("display-cycle").value = String(d.cycle);
}

function renderDisplay(status) {
  if (!config?.display.enabled) pill("pill-display", "Off");
  else if (status.display_on) pill("pill-display", "On", "good");
  else pill("pill-display", status.display_note?.startsWith("paused") ? "Paused" : "Not found", "warn");
  $("display-note").textContent = config?.display.enabled && status.display_note !== "off" ? status.display_note : "";
}

// ---------- fan lights ----------

// The same 12 combos as the service (core/src/lights.rs COMBOS): low, middle, high load.
const COMBOS = [
  ["Traffic Light", [0, 255, 0], [255, 255, 0], [255, 0, 0]],
  ["Ice to Fire", [0, 120, 255], [255, 255, 255], [255, 40, 0]],
  ["Ocean", [0, 255, 200], [0, 120, 255], [0, 0, 255]],
  ["Cyberpunk", [0, 255, 255], [180, 0, 255], [255, 0, 120]],
  ["Lava", [255, 120, 0], [255, 40, 0], [255, 0, 0]],
  ["Galaxy", [60, 0, 255], [160, 0, 255], [255, 0, 255]],
  ["Toxic", [0, 255, 0], [180, 255, 0], [255, 255, 0]],
  ["Sunset", [255, 160, 0], [255, 0, 100], [120, 0, 255]],
  ["Frost", [40, 200, 255], [0, 90, 255], [0, 20, 255]],
  ["Matrix to Alarm", [0, 120, 0], [0, 255, 80], [255, 0, 0]],
  ["Fire & Ice", [0, 140, 255], null, [255, 50, 0]],
  ["Blue & Purple", [0, 70, 255], [140, 0, 255], [220, 0, 255]],
];

// Static: quick picks beside "any color".
const SWATCHES = ["#ff0000", "#ff5a00", "#ffc800", "#00ff3c", "#00ffd0", "#00b4ff", "#0046ff", "#8c00ff", "#ff00c8", "#ff0050", "#ffffff"];
const SPECTRUM = ["#ff0000", "#ffff00", "#00ff00", "#00ffff", "#0000ff", "#ff00ff", "#ff0000"];
const RAINBOW_MAX = 8; // (core/src/config.rs RAINBOW_COLORS_MAX)
const RAINBOW_RATE = 24; // LEDs a second at Speed 100 % (core/src/lights.rs)
const PREVIEW_LEDS = 120; // the preview bar stands for this many LEDs
const hex = (c) => "#" + c.map((v) => v.toString(16).padStart(2, "0")).join("");
const comboColors = ([, low, mid, high]) => [low, mid, high].filter(Boolean).map(hex);

// The comet head's color at t (0-1) on a combo's colors, as the service picks
// it (core/src/lights.rs head): low -> mid -> high, or low -> high.
function comboAt(colors, t) {
  const rgb = colors.map((c) => [1, 3, 5].map((i) => parseInt(c.slice(i, i + 2), 16)));
  const [a, b, f] = rgb.length === 3 ? (t < 0.5 ? [rgb[0], rgb[1], t * 2] : [rgb[1], rgb[2], (t - 0.5) * 2]) : [rgb[0], rgb[1], t];
  return `rgb(${a.map((v, i) => Math.round(v + (b[i] - v) * f)).join(",")})`;
}

// Rainbow: the user's own colors, kept while "Full rainbow" is picked so
// switching back finds them again.
let ownColors = ["#ff0080", "#8c00ff", "#00c8ff"];

const lightsMode = () => document.querySelector('input[name="lights-mode"]:checked')?.value ?? "load";

// Each effect keeps its own Speed (one slider shows the one in use), so
// setting the rainbow fast never speeds up the comets, and back again.
let speeds = { load: 100, fixed: 100, rainbow: 100 };
let speedShown = null; // the key the slider shows now
const speedKey = () => (lightsMode() === "rainbow" ? "rainbow" : lightsMode() === "load" && !$("lights-comet-load").checked ? "fixed" : "load");
const paletteOwn = () => document.querySelector('input[name="lights-palette"]:checked')?.value === "own";

function readLights() {
  const leds = Number($("lights-leds").value.trim());
  return {
    ...config.lights,
    enabled: $("lights-enabled").checked,
    mode: lightsMode(),
    combo: Number($("lights-combo").value),
    comet_load: $("lights-comet-load").checked,
    comet_level: Number($("lights-comet-level").value),
    color: $("lights-color").value,
    // (own colors are saved even while Full rainbow is picked)
    rainbow: ownColors.slice(0, RAINBOW_MAX),
    rainbow_own: paletteOwn(),
    rainbow_length: Number($("lights-length").value),
    reverse: $("lights-reverse").checked,
    rainbow_load: $("lights-rainbow-load").checked,
    brightness: Number($("lights-brightness").value),
    speed: speeds.load,
    comet_speed: speeds.fixed,
    rainbow_speed: speeds.rainbow,
    leds_per_zone: Number.isInteger(leds) && leds >= 0 ? leds : config.lights.leds_per_zone,
    device: $("lights-device").value.trim(),
    zones: $("lights-zones").value.trim(),
  };
}

function fillLights() {
  const l = config.lights;
  $("lights-combo").replaceChildren(...COMBOS.map(([name], i) => new Option(`${i + 1}. ${name}`, String(i + 1))));
  $("lights-enabled").checked = l.enabled;
  for (const radio of document.querySelectorAll('input[name="lights-mode"]')) radio.checked = radio.value === l.mode;
  $("lights-combo").value = String(l.combo);
  $("lights-comet-load").checked = l.comet_load;
  $("lights-comet-level").value = String(l.comet_level);
  $("lights-color").value = l.color;
  if (l.rainbow.length) ownColors = [...l.rainbow];
  const own = (l.rainbow_own ?? l.rainbow.length > 0) && l.rainbow.length > 0;
  for (const radio of document.querySelectorAll('input[name="lights-palette"]')) {
    radio.checked = radio.value === (own ? "own" : "spectrum");
  }
  $("lights-length").value = String(l.rainbow_length);
  $("lights-reverse").checked = l.reverse;
  $("lights-rainbow-load").checked = l.rainbow_load;
  $("lights-brightness").value = String(l.brightness);
  speeds = { load: l.speed, fixed: l.comet_speed || l.speed, rainbow: l.rainbow_speed || l.speed };
  speedShown = null;
  $("lights-leds").value = String(l.leds_per_zone);
  $("lights-device").value = l.device;
  $("lights-zones").value = l.zones;
  showLights();
}

// The static swatches, the rainbow's own colors (each editable and
// removable, up to 8) and the palettes that fill them in one click.
function fillSwatches() {
  const color = $("lights-color").value;
  const swatches = SWATCHES.map((c) => {
    const b = document.createElement("button");
    b.type = "button";
    b.className = "swatch";
    b.style.setProperty("--c", c);
    b.title = c;
    b.setAttribute("aria-label", `Color ${c}`);
    b.setAttribute("aria-pressed", String(c === color));
    b.addEventListener("click", () => {
      $("lights-color").value = c;
      changed();
    });
    return b;
  });
  $("lights-swatches").replaceChildren(...swatches, $("lights-color"));

  const own = paletteOwn();
  $("lights-rainbow").hidden = !own;
  $("lights-palettes").hidden = !own;
  if (!own) return;
  const items = ownColors.map((c, i) => {
    const wrap = document.createElement("span");
    wrap.className = "own";
    const input = document.createElement("input");
    input.type = "color";
    input.value = c;
    input.title = `Color ${i + 1}`;
    input.addEventListener("input", () => {
      ownColors[i] = input.value;
      showLights(false);
      saveSoon();
    });
    wrap.append(input);
    if (ownColors.length > 2) {
      const x = document.createElement("button");
      x.type = "button";
      x.textContent = "×";
      x.title = "Remove this color";
      x.addEventListener("click", () => {
        ownColors.splice(i, 1);
        changed();
      });
      wrap.append(x);
    }
    return wrap;
  });
  if (ownColors.length < RAINBOW_MAX) {
    const add = document.createElement("button");
    add.type = "button";
    add.textContent = "+ Add";
    add.addEventListener("click", () => {
      ownColors.push(ownColors.at(-1) ?? "#ffffff");
      changed();
    });
    items.push(add);
  }
  $("lights-rainbow").replaceChildren(...items);

  $("lights-palettes").replaceChildren(
    ...COMBOS.map((combo) => {
      const colors = comboColors(combo);
      const b = document.createElement("button");
      b.type = "button";
      b.className = "palette";
      b.textContent = combo[0];
      b.title = `Use ${combo[0]}'s colors`;
      b.style.setProperty("--g", `linear-gradient(90deg, ${colors.join(", ")})`);
      b.addEventListener("click", () => {
        ownColors = colors;
        changed();
      });
      return b;
    }),
  );
}

// A click that changes the colors: show it and save.
function changed() {
  showLights();
  save();
}

// Color pickers fire while dragging: save once they settle, so the fans
// follow along without a file write per pixel.
let saveTimer = 0;
function saveSoon() {
  clearTimeout(saveTimer);
  saveTimer = setTimeout(save, 200);
}

// The rainbow's colors once round, back to the first.
const loopColors = () => (paletteOwn() ? [...ownColors, ownColors[0]] : SPECTRUM);

function showLights(redraw = true) {
  const mode = lightsMode();
  // Switched effect: the slider shows that effect's Speed; else it's the one moved.
  const key = speedKey();
  if (key !== speedShown) {
    $("lights-speed").value = String(speeds[key]);
    speedShown = key;
  } else {
    speeds[key] = Number($("lights-speed").value);
  }
  for (const row of document.querySelectorAll("[data-mode]")) row.hidden = !row.dataset.mode.split(" ").includes(mode);
  if (redraw) fillSwatches();
  const preview = $("lights-preview");
  preview.style.filter = `brightness(${0.35 + 0.65 * (Number($("lights-brightness").value) / 100)})`;
  if (mode === "static") {
    preview.style.background = $("lights-color").value;
  } else if (mode === "rainbow") {
    preview.style.background = `linear-gradient(90deg, ${loopColors().join(", ")})`;
    preview.style.backgroundSize = `${(Number($("lights-length").value) / PREVIEW_LEDS) * 100}% 100%`;
    preview.style.backgroundRepeat = "repeat-x";
  } else {
    const colors = comboColors(COMBOS[Number($("lights-combo").value) - 1] ?? COMBOS[11]);
    const gradient = `linear-gradient(90deg, ${colors.join(", ")})`;
    const fixed = !$("lights-comet-load").checked;
    // Fixed: the preview shows the one color the comets run in.
    preview.style.background = fixed ? comboAt(colors, Number($("lights-comet-level").value) / 100) : gradient;
    $("lights-comet-level").style.background = gradient;
    $("lights-level-row").hidden = !fixed;
    $("lights-comet-hint").textContent = fixed
      ? "Off: the comets run at the Speed below, in the Color you pick, whatever the CPU does."
      : "Comets run faster and hotter with CPU load.";
  }
  $("lights-brightness-value").textContent = `${$("lights-brightness").value} %`;
  $("lights-speed-value").textContent = `${$("lights-speed").value} %`;
  $("lights-length-value").textContent = `${$("lights-length").value} LEDs`;
  const fixedComets = mode === "load" && !$("lights-comet-load").checked;
  $("lights-speed-hint").textContent =
    mode === "rainbow"
      ? `${Math.round((RAINBOW_RATE * Number($("lights-speed").value)) / 100)} LEDs a second.`
      : fixedComets
        ? `${(Number($("lights-speed").value) / 100).toFixed(2)} comets a second.`
        : "100 % is cpu_rgb.py's speed. Lower is calmer.";
  if (mode === "rainbow") runPreview();
}

// The rainbow preview runs like the fans do (same speed and direction).
let previewAt = 0;
let previewFrame = 0;
function runPreview() {
  if (previewFrame) return;
  let last = performance.now();
  const tick = (now) => {
    const preview = $("lights-preview");
    if (lightsMode() !== "rainbow" || document.hidden) {
      previewFrame = 0;
      return;
    }
    const width = preview.clientWidth;
    const speed = (RAINBOW_RATE * Number($("lights-speed").value)) / 100;
    previewAt += ((now - last) / 1000) * speed * (width / PREVIEW_LEDS) * ($("lights-reverse").checked ? -1 : 1);
    last = now;
    preview.style.backgroundPosition = `${previewAt}px 0`;
    previewFrame = requestAnimationFrame(tick);
  };
  previewFrame = requestAnimationFrame(tick);
}
document.addEventListener("visibilitychange", () => !document.hidden && lightsMode() === "rainbow" && runPreview());

function renderLights(status) {
  const enabled = config?.lights.enabled;
  if (status.lights_on) pill("pill-lights", "On", "good");
  else if (enabled) pill("pill-lights", "Waiting", "warn");
  else pill("pill-lights", "Off");
  $("lights-note").textContent = enabled && status.lights_note !== "off" ? status.lights_note : "";
}

function fillForm() {
  $("enabled").checked = config.enabled;
  for (const radio of document.querySelectorAll('input[name="transport"]')) {
    radio.checked = radio.value === config.transport;
  }
  const fixed = config.wifi_target !== "";
  for (const radio of document.querySelectorAll('input[name="wifimode"]')) {
    radio.checked = radio.value === (fixed ? "fixed" : "auto");
  }
  $("wifi-target").value = config.wifi_target;
  $("control-key").value = config.control_key;
  $("interval").value = String(config.interval_ms);
  if (![...$("interval").options].some((o) => o.selected)) {
    $("interval").add(new Option(`Every ${config.interval_ms} ms`, String(config.interval_ms), true, true));
  }
  fillLights();
  fillDisplay();
  fillFans();
  syncVisibility();
}

function syncVisibility() {
  const transport = document.querySelector('input[name="transport"]:checked')?.value ?? "auto";
  const fixed = document.querySelector('input[name="wifimode"]:checked')?.value === "fixed";
  $("wifi-block").hidden = transport === "usb";
  $("usb-block").hidden = transport === "wifi";
  $("wifi-target").disabled = !fixed;
}

let savedTimer = 0;
async function save() {
  syncVisibility();
  const next = readForm();
  const wifiMode = document.querySelector('input[name="wifimode"]:checked')?.value;
  if (wifiMode === "fixed" && next.wifi_target === "") return; // wait for an address
  try {
    config = await invoke("save_config", { config: next });
    $("lights-leds").value = String(config.lights.leds_per_zone); // (a bad number goes back to the saved one)
    $("display-cycle").value = String(config.display.cycle);
    $("settings-error").hidden = true;
    $("saved").hidden = false;
    clearTimeout(savedTimer);
    savedTimer = setTimeout(() => ($("saved").hidden = true), 1500);
  } catch (error) {
    $("settings-error").textContent = String(error);
    $("settings-error").hidden = false;
  }
}

function setOptions(select, options, value = "") {
  value ??= "";
  select.replaceChildren(...options.map(([v, label]) => new Option(label, v)));
  if (value && !options.some(([v]) => v === value)) {
    select.add(new Option(`${value} (not found)`, value));
  }
  select.value = value;
}

function updatePorts(ports) {
  const key = JSON.stringify(ports);
  if (key === portsKey || document.activeElement === $("serial-port")) return;
  portsKey = key;
  const options = [["", "Auto-detect the clock (CH340)"]].concat(
    ports.map((p) => [p.name, p.description ? `${p.name} — ${p.description}` : p.name]),
  );
  setOptions($("serial-port"), options, config.serial_port);
}

async function updateFans() {
  const rows = await invoke("get_sensors");
  renderSensors(rows);
  updateFanControls(rows);
  const fans = rows.filter((r) => r.type.toLowerCase() === "fan" && !r.id.toLowerCase().includes("/gpu"));
  const key = JSON.stringify(fans.map((f) => f.id));
  if (key !== fansKey && document.activeElement !== $("fan-sensor")) {
    fansKey = key;
    const options = [["", "Auto (CPU fan header)"]].concat(
      fans.map((f) => [f.id, `${f.name} · ${Math.round(f.value)} rpm · ${f.hw || f.parent}`]),
    );
    setOptions($("fan-sensor"), options, config.fan_sensor);
  }
  updateTemps(rows);
}

const isDrive = (r) => /\/(nvme|ssd|hdd)\//i.test(r.id);
const isLimit = (r) => /warning|critical/i.test(r.name);

function updateTemps(rows) {
  const temps = rows.filter((r) => r.type.toLowerCase() === "temperature" && !isLimit(r));
  const key = JSON.stringify(temps.map((t) => t.id));
  const busy = [$("mobo-sensor"), $("ssd-sensor")].includes(document.activeElement);
  if (key === tempsKey || busy) return;
  tempsKey = key;
  const label = (t) => [t.id, `${t.name} · ${fmt(t.value)} °C · ${t.hw || t.parent}`];
  const board = temps.filter((t) => !isDrive(t) && !/\/(amdcpu|intelcpu|gpu)/i.test(t.id));
  setOptions($("mobo-sensor"), [["", "Auto (board System sensor)"]].concat(board.map(label)), config.mobo_sensor);
  setOptions($("ssd-sensor"), [["", "Auto (hottest drive)"]].concat(temps.filter(isDrive).map(label)), config.ssd_sensor);
}

// ---------- live status ----------

const fmt = (value, digits = 0) =>
  Number.isFinite(value) ? value.toLocaleString(undefined, { maximumFractionDigits: digits, minimumFractionDigits: digits }) : "–";
const maybe = (value, digits, unit) => (value > 0 ? `${fmt(value, digits)} ${unit}` : `– ${unit}`);

function tile(label, value, unit, detail) {
  const node = document.createElement("div");
  node.className = "tile";
  const l = document.createElement("div");
  l.className = "label";
  l.textContent = label;
  const v = document.createElement("div");
  v.className = "value";
  v.textContent = value;
  if (unit) {
    const u = document.createElement("span");
    u.className = "unit";
    u.textContent = unit;
    v.append(u);
  }
  const d = document.createElement("div");
  d.className = "detail";
  d.textContent = detail ?? "";
  node.append(l, v, d);
  return node;
}

function uptime(seconds) {
  const d = Math.floor(seconds / 86400);
  const h = Math.floor((seconds % 86400) / 3600);
  const m = Math.floor((seconds % 3600) / 60);
  return d ? `${d}d ${h}h` : `${h}h ${m}m`;
}

function renderTiles(s) {
  const power = s.pwrmode === "total" ? "measured total" : s.pwrmode === "parts" ? "CPU + GPU, not wall" : "no power sensor";
  $("tiles").replaceChildren(
    tile("CPU", fmt(s.cpu), "%", `${maybe(s.pf, 0, "MHz")} · ${maybe(s.cpuw, 0, "W")}`),
    tile("CPU temp", s.ct > 0 ? fmt(s.ct, 1) : "–", "°C", `${s.cores} cores / ${s.threads} threads`),
    tile("GPU", fmt(s.gpu), "%", `${maybe(s.gf, 0, "MHz")} · ${maybe(s.gpuw, 0, "W")}`),
    tile("GPU temp", s.gt > 0 ? fmt(s.gt, 0) : "–", "°C", `VRAM ${fmt(s.vram, 1)} / ${fmt(s.vramgb, 0)} GB`),
    tile("Memory", fmt(s.ram), "%", `of ${fmt(s.ramgb, 0)} GB · swap ${fmt(s.swap)}%`),
    tile("Board / SSD", `${s.mt > 0 ? fmt(s.mt) : "–"} / ${s.st > 0 ? fmt(s.st) : "–"}`, "°C", "board / hottest SSD"),
    tile("CPU fan", s.fan > 0 ? fmt(s.fan) : "–", "rpm", $("fan-sensor").selectedOptions[0]?.textContent.split(" · ")[0] ?? ""),
    tile("Power", s.sysw > 0 ? fmt(s.sysw) : "–", "W", power),
    tile("Network MB/s", `${fmt(s.nr, 1)} / ${fmt(s.nt, 1)}`, "", "down / up"),
    tile("Disk MB/s", `${fmt(s.dr, 1)} / ${fmt(s.dw, 1)}`, "", `read / write · ${fmt(s.free)} GB free`),
    tile("Uptime", uptime(s.up), "", `load ${fmt(s.load, 2)}`),
  );
}

function pill(id, text, tone) {
  const node = $(id);
  node.textContent = text;
  node.className = `pill ${tone ?? ""}`;
}

function renderStatus(view) {
  const { status, age, service } = view;
  const live = status && age !== null && age < 5 && service === "running";
  pill("pill-service", `Service ${service}`, service === "running" ? (live ? "good" : "warn") : "bad");

  let banner = "";
  if (service === "not installed") banner = "The Moni service is not installed. Run the Moni installer again.";
  else if (service !== "running") banner = "The Moni service is not running, so the clock gets no data. Try Restart service.";
  else if (!live) banner = "Waiting for the service to report…";
  else if (!status.sensors_ok) banner = `CPU temperature, power and fan are unavailable: ${status.sensors_note}`;
  $("banner").textContent = banner;
  $("banner").hidden = !banner;

  if (!status) {
    pill("pill-usb", "USB –");
    pill("pill-wifi", "WiFi –");
    return;
  }
  $("updated").textContent = live ? `v${status.version} · updated ${age}s ago` : `last update ${age}s ago`;
  if (status.sample) renderTiles(status.sample);

  const usbOn = status.enabled && status.transport !== "wifi";
  const wifiOn = status.enabled && status.transport !== "usb";
  if (!usbOn) pill("pill-usb", "USB off");
  else if (status.usb_port) pill("pill-usb", `USB ${status.usb_port}`, "good");
  else pill("pill-usb", status.usb_paused ? "USB paused" : "USB not plugged in", "warn");
  if (!wifiOn) pill("pill-wifi", "WiFi off");
  else if (status.wifi_targets.length) pill("pill-wifi", status.wifi_note.startsWith("direct") ? "WiFi direct" : "WiFi broadcast", "good");
  else pill("pill-wifi", "WiFi no network", "warn");

  updatePorts(status.ports ?? []);
  renderDiag(status);
  renderRestart(status);
  renderLights(status);
  renderDisplay(status);
  renderFans(status);
}

// "1792290000 restarted on COM3" -> shown for a minute after the click
let restartAskedAt = 0;
function renderRestart(status) {
  const [at, ...rest] = (status.restart_note || "").split(" ");
  const note = $("restart-note");
  const asked = restartAskedAt && Date.now() / 1000 - restartAskedAt < 60;
  if (!asked) return void (note.hidden = true);
  note.hidden = false;
  note.textContent = Number(at) >= restartAskedAt - 1 ? `Clock ${rest.join(" ")}.` : "Restarting the clock…";
}

function renderDiag(status) {
  const rows = [
    ["USB", status.usb_note || "–"],
    ["WiFi", status.wifi_note || "–"],
    ["GPU", status.gpu_note ? `${status.gpu_name} (${status.gpu_note})` : status.gpu_name],
    ["Sensors", status.sensors_ok ? "LibreHardwareMonitor OK" : status.sensors_note || "–"],
    ["Turn off", status.control_on ? status.control_note : "off (no key)"],
    ["Fan lights", status.lights_note || "–"],
    ["Digital display", status.display_note || "–"],
    ["Fan control", status.fans_note || "–"],
  ];
  $("diag").replaceChildren(
    ...rows.flatMap(([k, v]) => {
      const dt = document.createElement("dt");
      dt.textContent = k;
      const dd = document.createElement("dd");
      dd.textContent = v;
      return [dt, dd];
    }),
  );
}

function renderSensors(rows) {
  if (!rows.length) {
    $("sensors").textContent = "No sensor data yet.";
    return;
  }
  const table = document.createElement("table");
  for (const r of rows) {
    if (!["temperature", "fan", "control", "power", "clock", "load", "throughput"].includes(r.type.toLowerCase())) continue;
    const tr = table.insertRow();
    tr.insertCell().textContent = r.type;
    const v = tr.insertCell();
    v.className = "num";
    v.textContent = fmt(r.value, 1);
    tr.insertCell().textContent = `${r.name} · ${r.hw}`;
    const id = tr.insertCell();
    id.className = "id";
    id.textContent = r.id;
  }
  $("sensors").replaceChildren(table);
}

async function poll() {
  try {
    renderStatus(await invoke("get_status"));
  } catch (error) {
    pill("pill-service", "Service ?", "bad");
  }
}

// ---------- firmware ----------

async function flash() {
  if (flashing) return;
  flashing = true;
  $("flash").disabled = true;
  $("flash-log").hidden = false;
  $("flash-log").textContent = "";
  try {
    const message = await invoke("flash_firmware");
    appendLog(message);
  } catch (error) {
    appendLog(`Failed: ${error}`);
  } finally {
    flashing = false;
    $("flash").disabled = false;
  }
}

function appendLog(line) {
  const log = $("flash-log");
  // Keep one progress line instead of one per 10%.
  if (/^Writing\.\.\. /.test(line) && /Writing\.\.\. [^\n]*$/.test(log.textContent)) {
    log.textContent = log.textContent.replace(/Writing\.\.\. [^\n]*$/, line);
  } else {
    log.textContent += (log.textContent ? "\n" : "") + line;
  }
  log.scrollTop = log.scrollHeight;
}

// ---------- start ----------

async function start() {
  config = await invoke("get_config");
  fillForm();
  setOptions($("serial-port"), [["", "Auto-detect the clock (CH340)"]], config.serial_port);
  setOptions($("fan-sensor"), [["", "Auto (CPU fan header)"]], config.fan_sensor);
  setOptions($("mobo-sensor"), [["", "Auto (board System sensor)"]], config.mobo_sensor);
  setOptions($("ssd-sensor"), [["", "Auto (hottest drive)"]], config.ssd_sensor);
  $("autostart").checked = await invoke("get_autostart");

  for (const id of ["enabled", "serial-port", "fan-sensor", "mobo-sensor", "ssd-sensor", "interval", "control-key"]) {
    $(id).addEventListener("change", save);
  }
  for (const radio of document.querySelectorAll('input[name="transport"], input[name="wifimode"]')) {
    radio.addEventListener("change", save);
  }
  $("wifi-target").addEventListener("change", save);
  for (const id of ["fans-enabled", "fans-offset", "fans-hysteresis"]) $(id).addEventListener("change", save);
  for (const id of ["fans-offset", "fans-hysteresis"]) $(id).addEventListener("input", showFanValues);
  for (const node of document.querySelectorAll('#display-enabled, #display-mode, #display-cycle, .display-item, input[name="display-unit"], input[name="display-source"]')) {
    node.addEventListener("change", save);
  }
  $("display-cycle").addEventListener("keydown", (e) => e.key === "Enter" && save());
  for (const id of ["lights-enabled", "lights-combo", "lights-brightness", "lights-speed", "lights-length", "lights-reverse", "lights-rainbow-load", "lights-comet-load", "lights-comet-level", "lights-leds", "lights-device", "lights-zones"]) {
    $(id).addEventListener("change", save);
  }
  for (const id of ["lights-combo", "lights-brightness", "lights-speed", "lights-length", "lights-comet-level"]) $(id).addEventListener("input", () => showLights(false));
  $("lights-comet-load").addEventListener("change", () => showLights(false));
  for (const radio of document.querySelectorAll('input[name="lights-mode"], input[name="lights-palette"]')) {
    radio.addEventListener("change", changed);
  }
  $("lights-color").addEventListener("input", () => {
    showLights(false);
    saveSoon();
  });
  $("lights-color").addEventListener("change", () => showLights());
  for (const id of ["lights-leds", "lights-device", "lights-zones"]) {
    $(id).addEventListener("keydown", (e) => e.key === "Enter" && save());
  }
  $("wifi-target").addEventListener("keydown", (e) => e.key === "Enter" && save());
  $("autostart").addEventListener("change", async (e) => {
    try {
      e.target.checked = await invoke("set_autostart", { enabled: e.target.checked });
    } catch (error) {
      e.target.checked = !e.target.checked;
      $("settings-error").textContent = String(error);
      $("settings-error").hidden = false;
    }
  });

  const info = await invoke("firmware_info");
  if (info.available) $("fw-info").textContent = `Bundled image ${Math.round(info.size / 1024)} KB. Plug the clock in over USB, then update.`;
  else {
    $("fw-info").textContent = "No firmware image in this install.";
    $("flash").disabled = true;
  }
  $("flash").addEventListener("click", flash);
  await listen("flash-log", (event) => appendLog(event.payload));

  $("restart").addEventListener("click", async () => {
    $("restart").disabled = true;
    try {
      await invoke("restart_service");
    } catch (error) {
      $("settings-error").textContent = String(error);
      $("settings-error").hidden = false;
    }
    $("restart").disabled = false;
  });
  $("open-data").addEventListener("click", () => invoke("open_data_folder"));
  $("restart-clock").addEventListener("click", async () => {
    $("restart-clock").disabled = true;
    try {
      await invoke("restart_clock");
      restartAskedAt = Math.floor(Date.now() / 1000);
    } catch (error) {
      $("settings-error").textContent = String(error);
      $("settings-error").hidden = false;
    }
    setTimeout(() => ($("restart-clock").disabled = false), 3000);
  });

  poll();
  updateFans();
  setInterval(poll, 1000);
  setInterval(updateFans, 5000);
}

start();
