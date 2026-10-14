# WinDroidFS

> This application is still a work in progress. Do check back for updates if you're interested in the development of this application! See [WIP.md](WIP.md) for more information.

WinDroidFS bypasses the traditionally unstable Windows Media Transfer Protocol (MTP) by using the Android Open Accessory (AOA) protocol. It establishes a high-speed, raw binary pipe between your PC and your phone, treating your Android device like a standard local flash drive.

## Architecture

WinDroidFS is built as a Turborepo-powered monorepo consisting of 3 components:

- **WindroidFS for Windows (Rust, runs as a service):** A high-performance systems daemon that scans the USB bus, initiates the AOA hardware handshake, and mounts the incoming byte stream as a native Windows virtual drive.
- **Android Host (Kotlin, with support for Android >= 7.0):** A native Android application that receives the raw USB accessory intent, reads the local file system, and pipelines data directly to the PC.
- **Binary Protocol (Protobuf):** A typed, language-agnostic schema compiled into both Rust and Kotlin to dictate how file chunks and commands are requested over the raw endpoints.

## Connection Methods

WinDroidFS is designed to fall back gracefully if standard raw USB access is blocked by the host OS:

1. **Direct USB (via AOA):** The primary method. A high-speed raw pipe that requires zero setup on the Android side.
2. **ADB Forwarding:** A developer-focused transport layer utilizing USB Debugging.
3. **USB Tethering:** A fallback that routes TCP over a USB network interface, ensuring compatibility on heavily locked-down Windows machines.
4. **Local Wi-Fi:** A completely wireless transfer method using the local network.

## Configuration

The Android application allows the phone to control several settings:

- **Storage Scope:** The user can choose the folders they want to share with the PC, or the entire device storage.
- **Biometric Authentication:** The user can choose to enable or disable biometric authentication.
- **Foreground Service:** The user can choose to enable or disable the foreground service.

## Potential New Features

If you have additional ideas, feel free to open an issue or start a pull request.
These are some of the ideas I thought about but haven't actually started researching the feasbility of:

- Allowing rooted devices to access their root filesystem
