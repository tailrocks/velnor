[Index](https://www.freedesktop.org/software/systemd/man/latest/index.html)·
 [Directives](https://www.freedesktop.org/software/systemd/man/latest/systemd.directives.html) systemd develsystemd 262 (latest stable)systemd 261systemd 260systemd 259systemd 258systemd 257systemd 256systemd 255systemd 254systemd 253systemd 252systemd 251systemd 250systemd 249systemd 248systemd 247

* * *

## Name

systemd.syntax — General syntax of systemd configuration files

## Introduction [¶](https://www.freedesktop.org/software/systemd/man/latest/systemd.syntax.html?__goaway_challenge=meta-refresh&__goaway_id=f9edc13cad19c07c5307d22fc70ea9bc&__goaway_referer=https%3A%2F%2Fwww.google.com%2F\#Introduction "Permalink to this headline")

This page describes the basic principles of configuration files used by
[systemd(1)](https://www.freedesktop.org/software/systemd/man/latest/systemd.html#)
and related programs for:


- systemd unit files, see
[systemd.unit(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.unit.html#),
[systemd.service(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.service.html#),
[systemd.socket(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.socket.html#),
[systemd.device(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.device.html#),
[systemd.mount(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.mount.html#),
[systemd.automount(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.automount.html#),
[systemd.swap(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.swap.html#),
[systemd.target(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.target.html#),
[systemd.path(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.path.html#),
[systemd.timer(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.timer.html#),
[systemd.slice(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.slice.html#),
[systemd.scope(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.scope.html#)

- link files, see
[systemd.link(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.link.html#)

- netdev and network files, see
[systemd.netdev(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.netdev.html#),
[systemd.network(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.network.html#)

- daemon config files, see
[systemd-system.conf(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd-system.conf.html#),
[systemd-user.conf(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd-user.conf.html#),
[logind.conf(5)](https://www.freedesktop.org/software/systemd/man/latest/logind.conf.html#),
[journald.conf(5)](https://www.freedesktop.org/software/systemd/man/latest/journald.conf.html#),
[journal-remote.conf(5)](https://www.freedesktop.org/software/systemd/man/latest/journal-remote.conf.html#),
[journal-upload.conf(5)](https://www.freedesktop.org/software/systemd/man/latest/journal-upload.conf.html#),
[systemd-sleep.conf(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd-sleep.conf.html#),
[timesyncd.conf(5)](https://www.freedesktop.org/software/systemd/man/latest/timesyncd.conf.html#)

- nspawn files, see
[systemd.nspawn(5)](https://www.freedesktop.org/software/systemd/man/latest/systemd.nspawn.html#)


The syntax is inspired by
[XDG Desktop Entry Specification](https://standards.freedesktop.org/desktop-entry-spec/latest/)`.desktop` files, which are in turn inspired by Microsoft Windows
`.ini` files.


Each file is a plain text file divided into sections, with configuration entries in the style
_`key`_ = _`value`_. Whitespace immediately before or after
the "`=`" is ignored. Empty lines and lines starting with "`#`" or
"`;`" are ignored, which may be used for commenting.

Lines ending in a backslash are concatenated with the following line while reading and the
backslash is replaced by a space character. This may be used to wrap long lines. The limit on
line length is very large (currently 1 MB), but it is recommended to avoid such long lines and
use multiple directives, variable substitution, or other mechanism as appropriate for the given
file type. When a comment line or lines follow a line ending with a backslash, the comment block
is ignored, so the continued line is concatenated with whatever follows the comment block.

```
[Section A]
KeyOne=value 1
KeyTwo=value 2

# a comment

[Section B]
Setting="something" "some thing" "…"
KeyTwo=value 2 \
       value 2 continued

[Section C]
KeyThree=value 3\
# this line is ignored
; this line is ignored too
       value 3 continued
```

Boolean arguments used in configuration files can be written in
various formats. For positive settings the strings
`1`, `yes`, `true`
and `on` are equivalent. For negative settings, the
strings `0`, `no`,
`false` and `off` are
equivalent.

Time span values encoded in configuration files can be written in various formats. A stand-alone
number specifies a time in seconds. If suffixed with a time unit, the unit is honored. A
concatenation of multiple values with units is supported, in which case the values are added
up. Example: "`50`" refers to 50 seconds; "`2min 200ms`" refers to
2 minutes and 200 milliseconds, i.e. 120200 ms. The following time units are understood:
"`s`", "`min`", "`h`", "`d`",
"`w`", "`ms`", "`us`". For details see
[systemd.time(7)](https://www.freedesktop.org/software/systemd/man/latest/systemd.time.html#).

Various settings are allowed to be specified more than once, in which case the
interpretation depends on the setting. Often, multiple settings form a list, and setting to an
empty value "resets", which means that previous assignments are ignored. When this is allowed,
it is mentioned in the description of the setting. Note that using multiple assignments to the
same value makes the file incompatible with parsers for the XDG `.desktop`
file format.

## Quoting [¶](https://www.freedesktop.org/software/systemd/man/latest/systemd.syntax.html?__goaway_challenge=meta-refresh&__goaway_id=f9edc13cad19c07c5307d22fc70ea9bc&__goaway_referer=https%3A%2F%2Fwww.google.com%2F\#Quoting "Permalink to this headline")

For settings where quoting is allowed, the following general rules apply: double quotes ("…") and
single quotes ('…') may be used to wrap a whole item (the opening quote may appear only at the beginning
or after whitespace that is not quoted, and the closing quote must be followed by whitespace or the end
of line), in which case everything until the next matching quote becomes part of the same item. Quotes
themselves are removed. C-style escapes are supported. The table below contains the list of known escape
patterns. Only escape patterns which match the syntax in the table are allowed; other patterns may be
added in the future and unknown patterns will result in a warning. In particular, any backslashes should
be doubled. Finally, a trailing backslash ("`\`") may be used to merge lines, as described
above. UTF-8 is accepted, and hence typical unicode characters do not need to be escaped.

**Table 1. Supported escapes**

| Literal | Actual value |
| --- | --- |
| "`\a`" | bell |
| "`\b`" | backspace |
| "`\f`" | form feed |
| "`\n`" | newline |
| "`\r`" | carriage return |
| "`\t`" | tab |
| "`\v`" | vertical tab |
| "`\\`" | backslash |
| "`\"`" | double quotation mark |
| "`\'`" | single quotation mark |
| "`\s`" | space |
| "`\xxx`" | character number _`xx`_ in hexadecimal encoding |
| "`\nnn`" | character number _`nnn`_ in octal encoding |
| "`\unnnn`" | unicode code point _`nnnn`_ in hexadecimal encoding |
| "`\Unnnnnnnn`" | unicode code point _`nnnnnnnn`_ in hexadecimal encoding |

## See Also [¶](https://www.freedesktop.org/software/systemd/man/latest/systemd.syntax.html?__goaway_challenge=meta-refresh&__goaway_id=f9edc13cad19c07c5307d22fc70ea9bc&__goaway_referer=https%3A%2F%2Fwww.google.com%2F\#See%20Also "Permalink to this headline")

[systemd(1)](https://www.freedesktop.org/software/systemd/man/latest/systemd.html#), [systemd.time(7)](https://www.freedesktop.org/software/systemd/man/latest/systemd.time.html#)