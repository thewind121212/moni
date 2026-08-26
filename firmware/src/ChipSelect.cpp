#include "ChipSelect.h"

namespace {
constexpr uint8_t kDataPin = 4;
constexpr uint8_t kClockPin = 22;
constexpr uint8_t kLatchPin = 21;
}  // namespace

void ChipSelect::begin() {
  pinMode(kDataPin, OUTPUT);
  pinMode(kClockPin, OUTPUT);
  pinMode(kLatchPin, OUTPUT);
  digitalWrite(kDataPin, LOW);
  digitalWrite(kClockPin, LOW);
  digitalWrite(kLatchPin, LOW);
  writeMap(0);
}

void ChipSelect::writeMap(uint8_t enabledMap) {
  // SI HAI LCD chip-selects are active-low on bits 2..7 of a 74HC595.
  const uint8_t shifted = static_cast<uint8_t>((~enabledMap) << 2);
  digitalWrite(kLatchPin, LOW);
  shiftOut(kDataPin, kClockPin, LSBFIRST, shifted);
  digitalWrite(kLatchPin, HIGH);
}

void ChipSelect::setAll() { writeMap(0x3F); }

void ChipSelect::setPhysical(uint8_t leftToRightIndex) {
  if (leftToRightIndex > 5) return;
  // EleksTube's native digit numbering runs right-to-left.
  const uint8_t nativeIndex = 5 - leftToRightIndex;
  writeMap(static_cast<uint8_t>(1U << nativeIndex));
}
