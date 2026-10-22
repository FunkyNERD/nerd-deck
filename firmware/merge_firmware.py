# PlatformIO post-build step: merges the bootloader, partition table, boot_app0
# and app into one image to write at flash offset 0
# (.pio/build/<env>/firmware-merged.bin). The host app embeds it to update the
# device over its ROM bootloader. The flash mode/freq/size are the ones
# `pio run -t upload` passes to esptool, so the result matches an upload.
Import("env")  # noqa: F821


def merge(source, target, env):
    images = [(env.subst(off), env.subst(path)) for off, path in env["FLASH_EXTRA_IMAGES"]]
    images.append((env.subst("$ESP32_APP_OFFSET"), str(target[0])))
    out = env.subst("$BUILD_DIR/${PROGNAME}-merged.bin")
    args = [
        '"$PYTHONEXE"', '"$UPLOADER"', "--chip", env.subst("$BOARD_MCU"), "merge_bin",
        "--output", f'"{out}"',
        "--flash_mode", env.subst("${__get_board_flash_mode(__env__)}"),
        "--flash_freq", env.subst("${__get_board_f_image(__env__)}"),
        "--flash_size", env.BoardConfig().get("upload.flash_size", "detect"),
    ]
    for off, path in images:
        args += [off, f'"{path}"']
    env.Execute(" ".join(args))


env.AddPostAction("$BUILD_DIR/${PROGNAME}.bin", merge)  # noqa: F821
