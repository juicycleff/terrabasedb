# This is a simple script which creates a release build and
# moves the release builds into my $HOME/bin folder
cargo build --release
cp -f target/release/tdb target/release/tsh target/release/tdb-bench $HOME/bin
echo 'Done!'
