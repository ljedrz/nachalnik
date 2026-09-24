#!/bin/bash
# Starts a batch of sweeps in the background, one per scope file, named PREFIX-<scope>.
#
#   launch.sh KIND PREFIX SCOPEFILE...
#
# e.g. launch.sh audit a scopes/audit/*.txt   ->  a-kernel, a-context, ...
set -u
here=$(cd "$(dirname "$0")" && pwd)
kind=$1 prefix=$2; shift 2
for scope in "$@"; do
    name=$prefix-$(basename "$scope" .txt)
    nohup "$here/sweep.sh" "$kind" "$name" "$scope" > /dev/null 2>&1 &
    echo "$name"
done
