// Licensed under MIT OR Apache-2.0.
// Prints a line on the serial port, for the hardware in the loop test.

void setup() {
  Serial.begin(115200);
}

void loop() {
  Serial.println("Hello world!");
  delay(500);
}
