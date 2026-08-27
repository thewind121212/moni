#include <Arduino.h>
#include <ArduinoJson.h>
#include <NeoPixelBus.h>
#include <TFT_eSPI.h>
#include <WiFi.h>
#include <WiFiUdp.h>

#include "ChipSelect.h"

namespace {
constexpr uint8_t kPanelCount = 6;
constexpr uint16_t kWidth = 135;
constexpr uint16_t kHeight = 240;
constexpr uint8_t kBacklightPin = 2;
constexpr uint8_t kRgbPin = 32;
constexpr uint8_t kLeftButtonPin = 35;
constexpr uint8_t kModeButtonPin = 34;
constexpr uint8_t kRightButtonPin = 39;
constexpr uint8_t kPowerButtonPin = 36;
constexpr uint8_t kHistorySize = 45;
constexpr uint32_t kOfflineAfterMs = 3500;

// The mode button toggles the telemetry source: USB serial (wire) or WiFi
// UDP from the gaming PC. Fill in your 2.4 GHz network before building.
constexpr char kWifiSsid[] = "HomeLabLinh";
constexpr char kWifiPassword[] = "0918327132";
constexpr uint16_t kUdpPort = 5005;

constexpr uint16_t kBg = 0x0862;
constexpr uint16_t kSurface = 0x10E4;
constexpr uint16_t kSurface2 = 0x1946;
constexpr uint16_t kText = 0xEF9E;
constexpr uint16_t kMuted = 0x7BEF;
constexpr uint16_t kCyan = 0x05FF;
constexpr uint16_t kBlue = 0x4A5F;
constexpr uint16_t kGreen = 0x4FE8;
constexpr uint16_t kAmber = 0xFD84;
constexpr uint16_t kRed = 0xF9C7;
constexpr uint16_t kViolet = 0xA35F;

struct Metrics {
  bool windowsHost = false;
  uint8_t powerMode = 0;  // 0 unavailable, 1 CPU+GPU components, 2 measured total.
  float cpu = 0;
  float pcpu = 0;
  float ecpu = 0;
  float gpu = 0;
  float ram = 0;
  float swap = 0;
  float cpuTemp = 0;
  float gpuTemp = 0;
  float moboTemp = 0;
  float vrmTemp = 0;
  int pFreq = 0;
  int eFreq = 0;
  int gFreq = 0;
  int fan = 0;
  float systemW = 0;
  float cpuW = 0;
  float gpuW = 0;
  float disk = 0;
  float freeGb = 0;
  float netRx = 0;
  float netTx = 0;
  float diskRead = 0;
  float diskWrite = 0;
  float load = 0;
  uint32_t uptime = 0;
  uint32_t epoch = 0;
  uint8_t physicalCores = 0;
  uint8_t logicalCores = 0;
  float ramGb = 16;
};

// Tracks the peak of a signal over the last ~10 minutes (20 buckets x 30 s).
// Charts are scaled against this peak so the y-axis stays stable instead of
// collapsing every time the instantaneous value drops.
struct RollingPeak {
  static constexpr uint8_t kBuckets = 20;
  float buckets[kBuckets] = {};
  uint8_t index = 0;
  uint32_t lastRotateAt = 0;

  void add(float value, uint32_t now) {
    if (now - lastRotateAt >= 30000) {
      lastRotateAt = now;
      index = (index + 1) % kBuckets;
      buckets[index] = 0;
    }
    if (value > buckets[index]) buckets[index] = value;
  }

  float peak() const {
    float m = 0;
    for (uint8_t i = 0; i < kBuckets; ++i) m = max(m, buckets[i]);
    return m;
  }
};

TFT_eSPI tft;
TFT_eSprite canvas(&tft);
ChipSelect chipSelect;
NeoPixelBus<NeoGrbFeature, Neo800KbpsMethod> pixels(kPanelCount, kRgbPin);
Metrics metrics;

float histories[kPanelCount][kHistorySize] = {};
float historyMax[kPanelCount] = {100, 100, 100, 1, 50, 100};
RollingPeak netPeak;
RollingPeak powerPeak;
char serialLine[768];
size_t serialLength = 0;
uint32_t lastPacketAt = 0;
uint32_t lastDrawAt = 0;
uint32_t lastHistoryAt = 0;
uint8_t brightness = 220;
bool screensOn = true;
bool wifiMode = false;  // false = USB serial, true = WiFi UDP from the PC.
WiFiUDP udp;
char udpLine[768];
uint32_t ipShownAt = 0;  // Nonzero while the IP screen is up; auto-hides after 3 s.
IPAddress senderIp;      // Locked WiFi sender; released after kOfflineAfterMs of silence.
bool senderIsWin = false;

uint16_t statusColor(float value, float warm, float hot, uint16_t normal = kCyan) {
  if (value >= hot) return kRed;
  if (value >= warm) return kAmber;
  return normal;
}

void roundedCard(int16_t x, int16_t y, int16_t w, int16_t h) {
  canvas.fillRoundRect(x, y, w, h, 7, kSurface);
  canvas.drawRoundRect(x, y, w, h, 7, kSurface2);
}

void header(const char* label, uint16_t accent, bool online) {
  canvas.fillSprite(kBg);
  canvas.fillRect(0, 0, kWidth, 4, online ? accent : kRed);
  canvas.setTextDatum(TL_DATUM);
  canvas.setTextFont(2);
  canvas.setTextColor(kMuted, kBg);
  canvas.drawString(label, 8, 10);
  // Link icon: WiFi fan (amber while joining, green connected) or USB plug.
  const uint16_t link = !wifiMode                       ? kMuted
                        : WiFi.status() == WL_CONNECTED ? kGreen
                                                        : kAmber;
  if (wifiMode) {
    canvas.fillCircle(118, 21, 2, link);
    canvas.drawArc(118, 21, 6, 5, 135, 225, link, kBg);
    canvas.drawArc(118, 21, 10, 9, 135, 225, link, kBg);
  } else {
    // USB trident: stem with arrowhead, one square and one round branch.
    canvas.drawFastVLine(118, 12, 10, link);
    canvas.fillTriangle(115, 13, 121, 13, 118, 8, link);
    canvas.drawLine(118, 19, 113, 15, link);
    canvas.fillRect(111, 12, 3, 3, link);
    canvas.drawLine(118, 17, 123, 13, link);
    canvas.fillCircle(123, 12, 1, link);
    canvas.fillCircle(118, 23, 2, link);
  }
}

void bigValue(const String& value, const char* unit, uint16_t color) {
  canvas.setTextDatum(MC_DATUM);
  canvas.setTextFont(6);
  canvas.setTextColor(color, kBg);
  canvas.drawString(value, 65, 65);
  canvas.setTextDatum(TL_DATUM);
  canvas.setTextFont(2);
  canvas.setTextColor(kMuted, kBg);
  canvas.drawString(unit, 102, 77);
}

void metricRow(int y, const char* label, const String& value, uint16_t color = kText) {
  canvas.setTextDatum(TL_DATUM);
  canvas.setTextFont(2);
  canvas.setTextColor(kMuted, kSurface);
  canvas.drawString(label, 10, y);
  canvas.setTextDatum(TR_DATUM);
  canvas.setTextColor(color, kSurface);
  canvas.drawString(value, 125, y);
}

void progressBar(int y, float value, float maxValue, uint16_t color) {
  const int width = constrain(static_cast<int>(115.0f * value / maxValue), 0, 115);
  canvas.fillRoundRect(10, y, 115, 7, 3, kSurface2);
  if (width > 0) canvas.fillRoundRect(10, y, width, 7, 3, color);
}

void sparklineLine(const float* data, int y, uint16_t color, float scale) {
  const float s = max(scale, 0.01f);
  for (uint8_t i = 1; i < kHistorySize; ++i) {
    const int x0 = 8 + (i - 1) * 119 / (kHistorySize - 1);
    const int x1 = 8 + i * 119 / (kHistorySize - 1);
    const int y0 = y + 42 - constrain(static_cast<int>(data[i - 1] * 40 / s), 0, 40);
    const int y1 = y + 42 - constrain(static_cast<int>(data[i] * 40 / s), 0, 40);
    canvas.drawLine(x0, y0, x1, y1, color);
  }
}

void sparkline(uint8_t panel, int y, uint16_t color) {
  canvas.drawFastHLine(8, y + 42, 119, kSurface2);
  sparklineLine(histories[panel], y, color, historyMax[panel]);
}

void footer(const char* text) {
  canvas.setTextDatum(BC_DATUM);
  canvas.setTextFont(1);
  canvas.setTextColor(kMuted, kBg);
  canvas.drawString(text, 67, 237);
}

void drawCpu(bool online) {
  const uint16_t color = statusColor(metrics.cpu, 65, 88, kCyan);
  header("CPU", color, online);
  bigValue(String(metrics.cpu, 0), "%", color);
  roundedCard(5, 102, 125, 62);
  if (metrics.windowsHost) {
    metricRow(108, "CORES", String(metrics.physicalCores) + "C / " +
                               String(metrics.logicalCores) + "T", kBlue);
    progressBar(128, metrics.cpu, 100, kBlue);
    metricRow(140, "CLOCK", String(metrics.pFreq) + " MHz", kGreen);
  } else {
    metricRow(108, "P CORES", String(metrics.pcpu, 0) + "%", kBlue);
    progressBar(128, metrics.pcpu, 100, kBlue);
    metricRow(140, "E CORES", String(metrics.ecpu, 0) + "%", kGreen);
  }
  sparkline(0, 176, color);
  footer((String(metrics.cpuW, 1) + " W PACKAGE").c_str());
}

void drawMemory(bool online) {
  const uint16_t color = statusColor(metrics.ram, 75, 90, kViolet);
  header("MEMORY", color, online);
  bigValue(String(metrics.ram, 0), "%", color);
  roundedCard(5, 102, 125, 62);
  metricRow(108, "USED", String(metrics.ram * metrics.ramGb / 100.0f, 1) + " GB", color);
  progressBar(128, metrics.ram, 100, color);
  metricRow(140, "SWAP", String(metrics.swap, 0) + "%", kAmber);
  sparkline(1, 176, color);
  footer((String(metrics.ramGb, 0) +
          (metrics.windowsHost ? " GB DDR5" : " GB UNIFIED")).c_str());
}

void drawGpu(bool online) {
  const uint16_t color = statusColor(metrics.gpu, 70, 90, kBlue);
  header("GPU", color, online);
  bigValue(String(metrics.gpu, 0), "%", color);
  roundedCard(5, 102, 125, 62);
  metricRow(108, "POWER", String(metrics.gpuW, 0) + " W", kGreen);
  progressBar(128, metrics.gpu, 100, color);
  metricRow(140, "CLOCK", String(metrics.gFreq) + " MHz", kText);
  sparkline(2, 176, color);
}

void drawIo(bool online) {
  const float peak = max(max(metrics.netRx, metrics.netTx), 0.1f);
  const uint16_t color = kCyan;
  header("I/O", color, online);
  bigValue(String(peak, peak < 10 ? 1 : 0), "MB/s", color);
  roundedCard(5, 102, 125, 62);
  metricRow(108, "DOWNLOAD", String(metrics.netRx, 2), kGreen);
  metricRow(128, "UPLOAD", String(metrics.netTx, 2), kBlue);
  metricRow(148, "DISK R/W", String(metrics.diskRead, 1) + "/" + String(metrics.diskWrite, 1), kText);
  sparkline(3, 176, color);
  footer((String("PEAK ") + String(historyMax[3], historyMax[3] < 10 ? 1 : 0) + " MB/s").c_str());
}

String uptimeText(uint32_t seconds) {
  const uint32_t days = seconds / 86400;
  const uint32_t hours = (seconds % 86400) / 3600;
  return String(days) + "d " + String(hours) + "h";
}

// Warm/hot power thresholds depend on what the host reports. A Windows tower
// on an 850 W PSU idles near 100 W, so the Mac-scale numbers must not apply.
uint16_t powerColor() {
  if (!metrics.windowsHost) return statusColor(metrics.systemW, 30, 55, kAmber);
  if (metrics.powerMode == 2) return statusColor(metrics.systemW, 450, 650, kAmber);
  if (metrics.powerMode == 1) return statusColor(metrics.systemW, 250, 330, kAmber);
  return kMuted;  // No power sensor: never alarm on a stale value.
}

void drawSystem(bool online) {
  const uint16_t color = powerColor();
  header("POWER", color, online);
  if (metrics.windowsHost && metrics.powerMode == 0) {
    bigValue("N/A", "", kMuted);
  } else {
    bigValue(String(metrics.systemW, 1), "W", color);
  }
  roundedCard(5, 102, 125, 82);
  metricRow(108, "CPU FAN", metrics.fan > 0 ? String(metrics.fan) + " rpm" : "N/A", kCyan);
  metricRow(128, "DISK", String(metrics.disk, 0) + "%", kViolet);
  metricRow(148, "FREE", String(metrics.freeGb, 1) + " GB", kGreen);
  metricRow(168, "UPTIME", uptimeText(metrics.uptime), kText);
  sparkline(4, 188, color);
  if (!online) {
    if (!wifiMode) {
      footer("WAITING FOR PC (USB)");
    } else if (WiFi.status() == WL_CONNECTED) {
      // Point the PC at this address: monitor --udp <ip>.
      footer((String("WIFI ") + WiFi.localIP().toString() + ":" + String(kUdpPort)).c_str());
    } else {
      footer("WIFI CONNECTING...");
    }
  } else if (!metrics.windowsHost) {
    footer("M2 PRO  /  MONI");
  } else if (metrics.powerMode == 2) {
    footer("MEASURED TOTAL POWER");
  } else if (metrics.powerMode == 1) {
    footer("CPU + GPU / NOT WALL");
  } else {
    footer("POWER SENSOR MISSING");
  }
}

void tempRow(int y, const char* label, float value, float warm, float hot) {
  metricRow(y, label, value > 0 ? String(value, 0) + " C" : String("N/A"),
            value > 0 ? statusColor(value, warm, hot, kGreen) : kMuted);
}

void drawThermal(bool online) {
  const float hottest = max(metrics.cpuTemp, metrics.gpuTemp);
  const uint16_t color = statusColor(hottest, 70, 85, kGreen);
  header("THERMAL", color, online);
  bigValue(String(hottest, 0), "C", color);
  roundedCard(5, 102, 125, 82);
  tempRow(108, "CPU", metrics.cpuTemp, 70, 85);
  tempRow(128, "GPU", metrics.gpuTemp, 70, 85);
  tempRow(148, "BIOS", metrics.moboTemp, 55, 70);
  tempRow(168, "VRM", metrics.vrmTemp, 80, 95);
  sparkline(5, 188, color);
  footer(hottest >= 85 ? "HOT" : hottest >= 70 ? "WARM" : "THERMALS NORMAL");
}

void renderPanel(uint8_t panel, bool online) {
  switch (panel) {
    case 0: drawCpu(online); break;
    case 1: drawMemory(online); break;
    case 2: drawGpu(online); break;
    case 3: drawIo(online); break;
    case 4: drawSystem(online); break;
    case 5: drawThermal(online); break;
    default: return;
  }
  chipSelect.setPhysical(panel);
  canvas.pushSprite(0, 0);
}

RgbColor rgbFor(float value, float warm, float hot, const RgbColor& normal) {
  if (value >= hot) return RgbColor(235, 12, 28);
  if (value >= warm) return RgbColor(245, 105, 8);
  return normal;
}

void updateRgb(bool online) {
  const RgbColor colors[kPanelCount] = {
      rgbFor(metrics.cpu, 65, 88, RgbColor(0, 145, 225)),
      rgbFor(metrics.ram, 75, 90, RgbColor(125, 55, 225)),
      RgbColor(35, 100, 245),
      RgbColor(0, 220, 130),
      RgbColor(245, 85, 12),
      rgbFor(max(metrics.cpuTemp, metrics.gpuTemp), 70, 85, RgbColor(190, 190, 190)),
  };
  const float dim = brightness / 255.0f;
  for (uint8_t physical = 0; physical < kPanelCount; ++physical) {
    RgbColor c = online ? colors[physical] : RgbColor(70, 0, 0);
    c = RgbColor(static_cast<uint8_t>(c.R * dim),
                 static_cast<uint8_t>(c.G * dim),
                 static_cast<uint8_t>(c.B * dim));
    // The RGB chain follows the physical left-to-right slot order.
    pixels.SetPixelColor(physical, c);
  }
  pixels.Show();
}

void renderAll() {
  const bool online = lastPacketAt && millis() - lastPacketAt < kOfflineAfterMs;
  for (uint8_t panel = 0; panel < kPanelCount; ++panel) renderPanel(panel, online);
  updateRgb(online);
}

void pushHistory() {
  const uint32_t now = millis();
  const float values[kPanelCount] = {
      metrics.cpu,
      metrics.ram,
      metrics.gpu,
      max(metrics.netRx, metrics.netTx),
      metrics.systemW,
      max(metrics.cpuTemp, metrics.gpuTemp),
  };
  for (uint8_t panel = 0; panel < kPanelCount; ++panel) {
    memmove(&histories[panel][0], &histories[panel][1], sizeof(float) * (kHistorySize - 1));
    histories[panel][kHistorySize - 1] = values[panel];
  }
  // Scale I/O and power charts to the last 10 minutes' peak, so 1 MB/s draws
  // at 10% height when the 10-minute maximum was 10 MB/s.
  netPeak.add(values[3], now);
  powerPeak.add(values[4], now);
  historyMax[3] = max(1.0f, netPeak.peak());
  historyMax[4] = max(50.0f, powerPeak.peak());
}

void acceptJson(const char* line) {
  StaticJsonDocument<1024> doc;
  if (deserializeJson(doc, line)) return;
  if (doc["type"] == "hello") {
    lastPacketAt = millis();
    return;
  }
  if (doc["v"] != 1) return;

  const char* hostOs = doc["os"] | "";
  if (hostOs[0] != '\0') metrics.windowsHost = strcmp(hostOs, "win") == 0;
  const char* powerMode = doc["pwrmode"] | "";
  if (powerMode[0] != '\0') {
    metrics.powerMode = strcmp(powerMode, "total") == 0 ? 2
                        : strcmp(powerMode, "parts") == 0 ? 1 : 0;
  }

  metrics.cpu = doc["cpu"] | metrics.cpu;
  metrics.pcpu = doc["pc"] | metrics.pcpu;
  metrics.ecpu = doc["ec"] | metrics.ecpu;
  metrics.gpu = doc["gpu"] | metrics.gpu;
  metrics.ram = doc["ram"] | metrics.ram;
  metrics.swap = doc["swap"] | metrics.swap;
  metrics.cpuTemp = doc["ct"] | metrics.cpuTemp;
  metrics.gpuTemp = doc["gt"] | metrics.gpuTemp;
  metrics.moboTemp = doc["mt"] | metrics.moboTemp;
  metrics.vrmTemp = doc["vt"] | metrics.vrmTemp;
  metrics.pFreq = doc["pf"] | metrics.pFreq;
  metrics.eFreq = doc["ef"] | metrics.eFreq;
  metrics.gFreq = doc["gf"] | metrics.gFreq;
  metrics.fan = doc["fan"] | metrics.fan;
  metrics.systemW = doc["sysw"] | metrics.systemW;
  metrics.cpuW = doc["cpuw"] | metrics.cpuW;
  metrics.gpuW = doc["gpuw"] | metrics.gpuW;
  metrics.disk = doc["disk"] | metrics.disk;
  metrics.freeGb = doc["free"] | metrics.freeGb;
  metrics.netRx = doc["nr"] | metrics.netRx;
  metrics.netTx = doc["nt"] | metrics.netTx;
  metrics.diskRead = doc["dr"] | metrics.diskRead;
  metrics.diskWrite = doc["dw"] | metrics.diskWrite;
  metrics.load = doc["load"] | metrics.load;
  metrics.uptime = doc["up"] | metrics.uptime;
  metrics.epoch = doc["t"] | metrics.epoch;
  metrics.physicalCores = doc["cores"] | metrics.physicalCores;
  metrics.logicalCores = doc["threads"] | metrics.logicalCores;
  metrics.ramGb = doc["ramgb"] | metrics.ramGb;
  lastPacketAt = millis();
}

void readSerial() {
  while (Serial.available()) {
    const char c = static_cast<char>(Serial.read());
    if (c == '\n') {
      serialLine[serialLength] = '\0';
      if (serialLength) acceptJson(serialLine);
      serialLength = 0;
    } else if (c != '\r') {
      if (serialLength < sizeof(serialLine) - 1) serialLine[serialLength++] = c;
      else serialLength = 0;
    }
  }
}

void startWifi() {
  WiFi.mode(WIFI_STA);
  WiFi.setSleep(false);  // Keep 1 Hz UDP telemetry from stalling on modem sleep.
  WiFi.begin(kWifiSsid, kWifiPassword);
  udp.begin(kUdpPort);
}

void stopWifi() {
  udp.stop();
  WiFi.disconnect(true);
  WiFi.mode(WIFI_OFF);
}

bool packetFromWindows(const char* line) {
  StaticJsonDocument<32> filter;
  filter["os"] = true;
  StaticJsonDocument<96> peek;
  if (deserializeJson(peek, line, DeserializationOption::Filter(filter))) return false;
  return strcmp(peek["os"] | "", "win") == 0;
}

void readUdp() {
  // The host sends one JSON object per datagram, same lines as over serial.
  for (int size = udp.parsePacket(); size > 0; size = udp.parsePacket()) {
    const IPAddress from = udp.remoteIP();
    const int length = udp.read(udpLine, sizeof(udpLine) - 1);
    if (length <= 0) continue;
    udpLine[length] = '\0';
    if (udpLine[length - 1] == '\n') udpLine[length - 1] = '\0';
    // One sender at a time: the display locks to whoever feeds it. While the
    // lock holds, only a Windows sender may take over from a non-Windows one
    // (the gaming PC outranks the Mac fallback); anyone else is ignored.
    // kOfflineAfterMs of silence releases the lock.
    if (from != senderIp) {
      const bool locked = lastPacketAt && millis() - lastPacketAt < kOfflineAfterMs;
      if (locked && !(packetFromWindows(udpLine) && !senderIsWin)) continue;
      senderIp = from;
      senderIsWin = packetFromWindows(udpLine);
    }
    acceptJson(udpLine);
  }
}

bool buttonPressed(uint8_t pin) {
  static bool initialized[40] = {};
  static bool lastRaw[40] = {};
  static bool stableDown[40] = {};
  static uint32_t changedAt[40] = {};
  const bool rawDown = digitalRead(pin) == LOW;

  // Adopt the boot state without treating it as a button press. This matters
  // on SI HAI boards where an input may remain low while power settles.
  if (!initialized[pin]) {
    initialized[pin] = true;
    lastRaw[pin] = rawDown;
    stableDown[pin] = rawDown;
    changedAt[pin] = millis();
    return false;
  }

  if (rawDown != lastRaw[pin]) {
    lastRaw[pin] = rawDown;
    changedAt[pin] = millis();
  }

  if (rawDown != stableDown[pin] && millis() - changedAt[pin] >= 40) {
    stableDown[pin] = rawDown;
    return stableDown[pin];  // Only the released -> pressed edge fires.
  }
  return false;
}

// Full-screen notice on every panel (link mode changes, IP display).
void toast(const char* title, const char* subtitle, uint16_t color) {
  for (uint8_t panel = 0; panel < kPanelCount; ++panel) {
    canvas.fillSprite(kBg);
    canvas.fillRect(0, 0, kWidth, 5, color);
    canvas.setTextDatum(MC_DATUM);
    canvas.setTextFont(4);
    canvas.setTextColor(color, kBg);
    canvas.drawString(title, 67, 100);
    canvas.setTextFont(2);
    canvas.setTextColor(kMuted, kBg);
    canvas.drawString(subtitle, 67, 140);
    chipSelect.setPhysical(panel);
    canvas.pushSprite(0, 0);
  }
}

String ipText() {
  return WiFi.localIP().toString() + ":" + String(kUdpPort);
}

// Hold the connecting screen until the network joins. Give up after 15 s, or
// immediately when the mode button is pressed again; the caller falls back to
// the wire in both cases.
bool waitForWifi() {
  const uint32_t deadline = millis() + 15000;
  while (millis() < deadline) {
    if (WiFi.status() == WL_CONNECTED) return true;
    if (buttonPressed(kModeButtonPin)) return false;
    delay(50);
  }
  return false;
}

void handleButtons() {
  if (buttonPressed(kPowerButtonPin)) {
    screensOn = !screensOn;
    chipSelect.setAll();
    tft.writecommand(screensOn ? 0x29 : 0x28);
    // SI HAI display power is active-high.
    digitalWrite(kBacklightPin, screensOn ? HIGH : LOW);
    if (screensOn) {
      renderAll();
    } else {
      // Screens off means fully dark: the RGB chain behind the panels too.
      pixels.ClearTo(RgbColor(0));
      pixels.Show();
    }
  }
  const bool leftPressed = buttonPressed(kLeftButtonPin);
  const bool rightPressed = buttonPressed(kRightButtonPin);
  if (wifiMode && WiFi.status() == WL_CONNECTED && screensOn &&
      (leftPressed || rightPressed)) {
    // Up/down shows the device IP; press again (or wait 3 s) to go back.
    if (ipShownAt) {
      ipShownAt = 0;
      renderAll();
    } else {
      toast("WIFI", ipText().c_str(), kGreen);
      ipShownAt = millis();
    }
  } else {
    if (leftPressed && brightness > 40) brightness -= 30;
    if (rightPressed && brightness < 225) brightness += 30;
  }

  if (buttonPressed(kModeButtonPin)) {
    wifiMode = !wifiMode;
    lastPacketAt = 0;  // Show offline until the new source delivers a packet.
    ipShownAt = 0;
    if (wifiMode) {
      startWifi();
      if (screensOn) toast("WIFI", "CONNECTING...", kAmber);
      if (waitForWifi()) {
        if (screensOn) {
          toast("WIFI", ipText().c_str(), kGreen);
          delay(1500);
        }
      } else {
        stopWifi();
        wifiMode = false;
        if (screensOn) {
          toast("USB", "FALLBACK TO WIRE", kRed);
          delay(1500);
        }
      }
    } else {
      stopWifi();
      if (screensOn) {
        toast("USB", "SERIAL LINK", kCyan);
        delay(700);
      }
    }
    if (screensOn) renderAll();
  }
}

void splash() {
  // Boot LED self-test: all backlights full white. The WS2812 data line
  // daisy-chains through every display module, so a dark LED here (and all
  // LEDs after it) means that module is not seated properly.
  pixels.ClearTo(RgbColor(255, 255, 255));
  pixels.Show();
  const char* labels[kPanelCount] = {"M", "O", "N", "I", "M", "2"};
  const uint16_t colors[kPanelCount] = {kCyan, kViolet, kBlue, kGreen, kAmber, kRed};
  for (uint8_t panel = 0; panel < kPanelCount; ++panel) {
    canvas.fillSprite(kBg);
    canvas.fillRect(0, 0, kWidth, 5, colors[panel]);
    canvas.setTextDatum(MC_DATUM);
    canvas.setTextFont(6);
    canvas.setTextColor(colors[panel], kBg);
    canvas.drawString(labels[panel], 67, 105);
    canvas.setTextFont(2);
    canvas.setTextColor(kMuted, kBg);
    canvas.drawString("SYSTEM DISPLAY", 67, 165);
    chipSelect.setPhysical(panel);
    canvas.pushSprite(0, 0);
  }
}
}  // namespace

void setup() {
  // Let the 5 V rail settle before touching the panels. On a PC the USB
  // enumeration delays boot enough to mask this; on a wall adapter the ESP32
  // boots instantly and the displays stay blank without the wait. The vendor
  // firmware waits 500 ms here too.
  delay(500);
  Serial.begin(115200);
  Serial.setRxBufferSize(2048);

  pinMode(kBacklightPin, OUTPUT);
  digitalWrite(kBacklightPin, LOW);
  pinMode(kLeftButtonPin, INPUT_PULLUP);
  pinMode(kModeButtonPin, INPUT_PULLUP);
  pinMode(kRightButtonPin, INPUT_PULLUP);
  pinMode(kPowerButtonPin, INPUT_PULLUP);

  chipSelect.begin();
  chipSelect.setAll();
  tft.writecommand(0x01);
  delay(150);
  tft.init();
  tft.setRotation(0);
  chipSelect.setAll();
  tft.fillScreen(kBg);
  digitalWrite(kBacklightPin, HIGH);

  canvas.setColorDepth(16);
  canvas.createSprite(kWidth, kHeight);
  pixels.Begin();
  pixels.ClearTo(RgbColor(0));
  pixels.Show();

  splash();
  delay(600);
  renderAll();
  Serial.println("MONI_READY");
}

void loop() {
  if (wifiMode) readUdp();
  else readSerial();
  handleButtons();

  const uint32_t now = millis();
  if (now - lastHistoryAt >= 1000) {
    lastHistoryAt = now;
    pushHistory();
  }
  if (ipShownAt && now - ipShownAt >= 3000) {
    ipShownAt = 0;
    if (screensOn) renderAll();
  }
  if (screensOn && !ipShownAt && now - lastDrawAt >= 1000) {
    lastDrawAt = now;
    renderAll();
  }
  delay(2);
}
