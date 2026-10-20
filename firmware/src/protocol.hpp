#pragma once

// Newline-delimited JSON over USB CDC. See docs/PROTOCOL.md.
namespace protocol {

void init();

// Drain the serial RX buffer and dispatch any complete messages.
void poll();

bool isConnected();

void sendHello();
void sendPress(const char* id);

}  // namespace protocol
