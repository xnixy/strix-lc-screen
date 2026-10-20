Windows and Linux builds. ffmpeg is included.

A small app for the ROG Strix LC IV / SLC IV pump screen (USB 0B05:1DE7). It talks to the panel directly. On Windows, quit "ROG STRIX LC & SLC IV Series" before you open it. Both programs want the same USB connection.

- strix-lc-screen-windows-x86_64.zip. Unzip it and run strix-lc-screen.exe. Keep ffmpeg.exe in that folder.
- strix-lc-screen-linux-x86_64.tar.gz. The program, ffmpeg, and a udev rule.
- strix-lc-screen_{{version}}_amd64.deb for Debian and Ubuntu.

The package installs the program and the USB permission. After it is installed, unplug the cooler and plug it back in, then run strix-lc-screen. The tarball has the same program, plus ffmpeg and the udev rule, if you would rather not use the package. Copy that rule, reload udev, and unplug the cooler once.

New in 0.2.1:

- The time and date show on the stats band over a photo or video. They get their own line, so all 6 readings still fit.

New in 0.2.0:

- A stats panel, drawn the way the ASUS app does it. Show it as a band over your photo or video, or as a full dashboard. Pick up to 6 readings plus the time and date: CPU usage and clock, GPU usage, clock, temperature, power, fan, and memory (NVIDIA cards), RAM, disk space and speed, and network speed.
- CPU temperature and power, pump speed, coolant temperature, and fan speed when HWiNFO is running with Shared Memory Support turned on. Windows does not let programs read these without a driver.
- Turn the screen on or off, and choose whether it plays the standby clip while the computer sleeps. These are the ASUS app's switches, and the pump remembers them.
- Put the pump screen to sleep while the computer is locked.
- Go back to the pump's own animation from the window.
- The picture recovers by itself if the pump stops taking frames, for example after the screen is turned off and on.

What you can do:

- Set brightness, rotation (0°, 90°, 180°, 270°), and wake or sleep the screen.
- Show a photo or a short video, with a crop box and a zoom slider.
- Change playback speed from 0.25× to 2× while the clip is running. 1× matches the length of the video.
- Close the window and leave the clip running from the tray icon.
- Start when you sign in, stay in the tray, and send the last file again on its own.

Click the tray icon to open the window. Right-click it to stop the video or quit. Stop video leaves the last frame on the pump. Quit exits the program.

The cooler does not store your clip or the stats panel. Pictures are sent as JPEG frames from this computer, and playback continues while this program is running, including from the tray. After you quit, the pump goes back to its built-in animation. Video is limited to about 20 seconds, sampled at up to 8 frames per second.

The pump and fans are not controlled over this USB connection. They run from the motherboard headers.

Settings are saved in %APPDATA%\strix-lc-screen\settings.json on Windows, and in ~/.config/strix-lc-screen/settings.json on Linux. Settings from 0.1.x carry over.

The stats panel uses the Rajdhani font, under the SIL Open Font License. FONT-LICENSE.txt in the download has the license.
