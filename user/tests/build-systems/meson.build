# Built by cargo xtask with the sysroot's Meson cross file.
project('tally', 'c')

tally = shared_library('tally-meson', 'tally.c',
  c_args: '-DBUILD_SYSTEM="meson"', version: '1.2.3', soversion: '1',
  install: true)
count = static_library('count-meson', 'count.c', install: true)
executable('tally-meson-user', 'main.c', link_with: [tally, count],
  install: true)

# Libraries in the sysroot are found with their Mikros suffixes.
cc = meson.get_compiler('c')
cc.find_library('c', static: false)
cc.find_library('unwind', static: true)
