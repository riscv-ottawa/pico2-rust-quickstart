# CYW43439 firmware

These are the firmware blobs the Pico 2 W build loads into its CYW43439 radio at boot:

* `43439A0.bin` is the Wi-Fi firmware.
* `43439A0_clm.bin` is the country locale matrix, the regulatory data the radio needs before it will transmit.
* `nvram_rp2040.bin` is the radio's board configuration. It was written for the original Pico W, and embassy's Pico 2 W examples use the same file.

They are copied from the [embassy repository](https://github.com/embassy-rs/embassy/tree/main/cyw43-firmware), which obtained them from [georgerobotics/cyw43-driver](https://github.com/georgerobotics/cyw43-driver/tree/main/firmware).

Licensed under the [Infineon Permissive Binary License](./LICENSE-permissive-binary-license-1.0.txt).
