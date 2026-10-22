#pragma once

#define LGFX_USE_V1
#include <LovyanGFX.hpp>

// Panel/touch wiring for the Waveshare ESP32-C5-Touch-LCD-3.5: an ST7796 over
// SPI and an FT6336 on I2C, with the LCD and touch resets and the backlight
// behind a CH32V003 I/O expander on the same I2C bus. Pins are from
// Waveshare's examples (github.com/waveshareteam/ESP32-C5-Touch-LCD-3.5).
namespace board {

constexpr int kI2cPort = 0;
constexpr int kSda = 27;
constexpr int kScl = 26;

// The expander's registers: pin directions (1 = output), output levels, and
// the backlight's PWM duty (0-255).
constexpr uint8_t kExpander = 0x24;
constexpr uint8_t kRegMode = 0x02;
constexpr uint8_t kRegOutput = 0x03;
constexpr uint8_t kRegPwm = 0x05;
constexpr uint8_t kTouchReset = 1 << 0;
constexpr uint8_t kLcdReset = 1 << 1;
constexpr uint8_t kPower = 1 << 5;

inline void expanderWrite(uint8_t reg, uint8_t value) {
    lgfx::i2c::writeRegister8(kI2cPort, kExpander, reg, value);
}

// Backlight through the expander's PWM output.
class Light : public lgfx::ILight {
public:
    bool init(uint8_t brightness) override {
        setBrightness(brightness);
        return true;
    }
    void setBrightness(uint8_t brightness) override { expanderWrite(kRegPwm, brightness); }
};

// Powers the panel and pulses the LCD and touch resets, before tft.init().
inline void powerUp() {
    lgfx::i2c::init(kI2cPort, kSda, kScl);
    expanderWrite(kRegMode, 0xFF);
    expanderWrite(kRegOutput, 0x00);
    expanderWrite(kRegPwm, 0x00);
    expanderWrite(kRegMode, kTouchReset | kLcdReset | kPower);
    expanderWrite(kRegOutput, kTouchReset | kLcdReset);
    delay(50);
    expanderWrite(kRegOutput, 0x00);
    delay(50);
    expanderWrite(kRegOutput, kTouchReset | kLcdReset | kPower);
    delay(120);
}

class Lcd : public lgfx::LGFX_Device {
    lgfx::Panel_ST7796 panel_;
    lgfx::Bus_SPI bus_;
    Light light_;
    lgfx::Touch_FT5x06 touch_;

public:
    Lcd() {
        {
            auto cfg = bus_.config();
            cfg.spi_host = SPI2_HOST;
            cfg.spi_mode = 0;
            cfg.freq_write = 80000000;
            cfg.freq_read = 16000000;
            cfg.spi_3wire = false;
            cfg.use_lock = true;
            cfg.dma_channel = SPI_DMA_CH_AUTO;
            cfg.pin_sclk = 6;
            cfg.pin_mosi = 7;
            cfg.pin_miso = 2;
            cfg.pin_dc = 5;
            bus_.config(cfg);
            panel_.setBus(&bus_);
        }
        {
            auto cfg = panel_.config();
            cfg.pin_cs = 8;
            cfg.pin_rst = -1;  // on the expander; see powerUp()
            cfg.pin_busy = -1;
            cfg.panel_width = 320;
            cfg.panel_height = 480;
            cfg.offset_x = 0;
            cfg.offset_y = 0;
            cfg.offset_rotation = 0;
            cfg.readable = false;
            cfg.invert = true;
            cfg.rgb_order = false;
            cfg.dlen_16bit = false;
            cfg.bus_shared = false;
            panel_.config(cfg);
        }
        panel_.setLight(&light_);
        {
            auto cfg = touch_.config();
            cfg.x_min = 0;
            cfg.x_max = 319;
            cfg.y_min = 0;
            cfg.y_max = 479;
            cfg.pin_int = 3;
            cfg.bus_shared = true;  // with the expander
            cfg.offset_rotation = 0;
            cfg.i2c_port = kI2cPort;
            cfg.i2c_addr = 0x38;
            cfg.pin_sda = kSda;
            cfg.pin_scl = kScl;
            cfg.freq = 400000;
            touch_.config(cfg);
            panel_.setTouch(&touch_);
        }
        setPanel(&panel_);
    }
};

}  // namespace board
