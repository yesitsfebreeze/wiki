build:
  cargo build --release
  #!/bin/bash
  mkdir -p bin
  if [ -f target/release/wiki.exe ]; then cp target/release/wiki.exe bin/wiki.exe; else cp target/release/wiki bin/wiki; fi
