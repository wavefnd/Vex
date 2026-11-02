{ format = 5, name = "wave_bridge", version = "0.1.0",
  targets = { lib = { output = "static", mode = "freestanding", relocation = "pic" } },
  target_config = [
    { triple = "x86_64-pc-windows-msvc", lib = { relocation = "default" } },
    { triple = "aarch64-pc-windows-msvc", lib = { relocation = "default" } }
  ],
  features = { tuned = [] },
  dependencies = [{ name = "math", path = "../math" }] }
