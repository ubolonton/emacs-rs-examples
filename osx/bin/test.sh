#!/usr/bin/env bash

# FIX: Use Rust instead of bash to drive the tests.

system=`uname`
if [[ $system == "Linux" ]]; then
    ext="so"
elif [[ $system == "Darwin" ]]; then
    ext="dylib"
else
    echo "Unsupported system: $system"
    exit 1
fi

here=`cd $(dirname $BASH_SOURCE); pwd`
root=`cd $here/..; pwd`
MODULE_DIR=$root/target/debug

# test-module
MODULE_ORIGINAL=libemacs_osx.$ext
MODULE_RENAMED=osx.so
`cd $MODULE_DIR && ln -f -s $MODULE_ORIGINAL $MODULE_RENAMED`

EMACS=emacs

RUST_BACKTRACE=0 $EMACS -batch -l ert \
              -l $MODULE_DIR/$MODULE_RENAMED \
              -l $root/src/test.el \
              -f ert-run-tests-batch-and-exit
