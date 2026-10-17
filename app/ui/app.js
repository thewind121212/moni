// Moni app UI. Talks to the Rust side through Tauri commands; the service
// itself never talks to this window, it just writes status.json.
const { invoke } = window.__TAURI__.core;
const { listen } = window.__TAURI__.event;

const $ = (id) => document.getElementById(id);
let config = null;
let portsKey = "";
let fansKey = "";
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
    interval_ms: Number($("interval").value),
  };
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
  $("interval").value = String(config.interval_ms);
  if (![...$("interval").options].some((o) => o.selected)) {
    $("interval").add(new Option(`Every ${config.interval_ms} ms`, String(config.interval_ms), true, true));
  }
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
    $("settings-error").hidden = true;
    $("saved").hidden = false;
    clearTimeout(savedTimer);
    savedTimer = setTimeout(() => ($("saved").hidden = true), 1500);
  } catch (error) {
    $("settings-error").textContent = String(error);
    $("settings-error").hidden = false;
  }
}

function setOptions(select, options, value) {
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
  const fans = rows.filter((r) => r.type.toLowerCase() === "fan" && !r.id.toLowerCase().includes("/gpu"));
  const key = JSON.stringify(fans.map((f) => f.id));
  if (key === fansKey || document.activeElement === $("fan-sensor")) return;
  fansKey = key;
  const options = [["", "Auto (CPU fan header)"]].concat(
    fans.map((f) => [f.id, `${f.name} · ${Math.round(f.value)} rpm · ${f.hw || f.parent}`]),
  );
  setOptions($("fan-sensor"), options, config.fan_sensor);
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
}

function renderDiag(status) {
  const rows = [
    ["USB", status.usb_note || "–"],
    ["WiFi", status.wifi_note || "–"],
    ["GPU", status.gpu_note ? `${status.gpu_name} (${status.gpu_note})` : status.gpu_name],
    ["Sensors", status.sensors_ok ? "LibreHardwareMonitor OK" : status.sensors_note || "–"],
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
    if (!["temperature", "fan", "power", "clock", "load", "throughput"].includes(r.type.toLowerCase())) continue;
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
  $("autostart").checked = await invoke("get_autostart");

  for (const id of ["enabled", "serial-port", "fan-sensor", "interval"]) $(id).addEventListener("change", save);
  for (const radio of document.querySelectorAll('input[name="transport"], input[name="wifimode"]')) {
    radio.addEventListener("change", save);
  }
  $("wifi-target").addEventListener("change", save);
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

  poll();
  updateFans();
  setInterval(poll, 1000);
  setInterval(updateFans, 5000);
}

start();
