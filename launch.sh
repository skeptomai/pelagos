#!/bin/sh
ALPINE_BUILD=example-20220710.tar.gz
cp ../alpine-make-rootfs/$ALPINE_BUILD ./ && sudo rm -fr ./alpine-rootfs && mkdir ./alpine-rootfs && cd ./alpine-rootfs/ && tar zxvf ../$ALPINE_BUILD ./ && cd ..
# export RUST_LOG=info
# export RUST_BACKTRACE=full
sudo ./target/debug/remora --exe /bin/ash --rootfs ./alpine-rootfs