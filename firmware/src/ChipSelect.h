#pragma once

#include <Arduino.h>

class ChipSelect {
 public:
  void begin();
  void setAll();
  void setPhysical(uint8_t leftToRightIndex);

 private:
  void writeMap(uint8_t enabledMap);
};

