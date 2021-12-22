

## build x87

rtd1296:
source $HOME/.cargo/env

## build arm ok:

build-arm-in-docker.sh

## build arm but failed:

```
#sudo apt install gcc-arm-linux-gnueabi

CROSS_TOOLCHAINS_DIR=/projects/openwrt/arm-fw867-linux-uclibcgnueabi/
export PATH=$PATH:$STAGING_DIR/host/bin:$CROSS_TOOLCHAINS_DIR/bin

sudo apt install gcc-arm-linux-gnueabi

rustup target add arm-unknown-linux-gnueabi

mkdir .cargo

cat <<-EOF >.cargo/config
[target.arm-unknown-linux-gnueabi]
linker="arm-linux-gnueabi-gcc"
rustflags = ["-C", "target-feature=+crt-static"]
EOF

CROSS_COMPILE=arm-linux-gnueabi- cargo build --target arm-unknown-linux-gnueabi --example hyper

rustup target add arm-fw867-linux-uclibcgnueabi

mkdir -p .cargo

if [ ! -f ".cargo/config" ]; then

cat <<-EOF >.cargo/config
[target.arm-fw867-linux-uclibcgnueabi]
linker="arm-fw867-linux-uclibcgnueabi-gcc"
rustflags = ["-C", "target-feature=+crt-static"]
EOF

fi

CROSS_COMPILE=arm-fw867-linux-uclibcgnueabi- cargo build --target arm-fw867-linux-uclibcgnueabi-gcc --example hyper

```
