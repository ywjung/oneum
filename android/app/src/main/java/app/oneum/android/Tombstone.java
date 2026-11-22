package app.oneum.android;

import java.io.IOException;
import java.nio.charset.StandardCharsets;
import java.util.ArrayList;
import java.util.HashMap;
import java.util.List;
import java.util.Locale;
import java.util.Map;

/**
 * Reads what matters of the record Android keeps of a crash of the native side (a
 * "tombstone", which ApplicationExitInfo hands out since Android 12): the signal, what
 * the process said when it gave up, and where the thread that crashed was. The record
 * is a protocol buffer (system/core/debuggerd/proto/tombstone.proto in Android's
 * sources); only the fields named here are read, everything else is skipped.
 */
final class Tombstone {
    private Tombstone() {
    }

    /** A stretch of the record, read from front to back. */
    private static final class Reader {
        private final byte[] bytes;
        private int at;
        private final int end;
        /** The kind of the field {@link #field} read last: 0 a number, 2 a stretch. */
        int kind;

        Reader(byte[] bytes, int at, int end) {
            this.bytes = bytes;
            this.at = at;
            this.end = end;
        }

        boolean more() {
            return at < end;
        }

        private IOException bad(String what) {
            return new IOException(what + ", at byte " + at + " of " + bytes.length);
        }

        long number() throws IOException {
            long value = 0;
            for (int shift = 0; shift < 64; shift += 7) {
                if (at >= end) {
                    throw bad("the record ends in a number");
                }
                byte part = bytes[at++];
                value |= (long) (part & 0x7f) << shift;
                if (part >= 0) {
                    return value;
                }
            }
            throw bad("a number of the record is too long");
        }

        /** The number of the next field; its kind is in {@link #kind}. */
        int field() throws IOException {
            long tag = number();
            kind = (int) (tag & 7);
            return (int) (tag >>> 3);
        }

        private int length() throws IOException {
            long length = number();
            if (length < 0 || length > end - at) {
                throw bad("a field of the record is longer than the record");
            }
            return (int) length;
        }

        Reader stretch() throws IOException {
            int length = length();
            Reader inner = new Reader(bytes, at, at + length);
            at += length;
            return inner;
        }

        String text() throws IOException {
            int length = length();
            String text = new String(bytes, at, length, StandardCharsets.UTF_8);
            at += length;
            return text;
        }

        void skip() throws IOException {
            switch (kind) {
                case 0:
                    number();
                    break;
                case 1:
                    at += 8;
                    break;
                case 2: {
                    // (Not `at += length()`: Java reads `at` before the length moves it.)
                    int length = length();
                    at += length;
                    break;
                }
                case 5:
                    at += 4;
                    break;
                default:
                    throw bad("a field of the record is of no known kind (" + kind + ")");
            }
            if (at > end) {
                throw bad("the record ends in a field");
            }
        }
    }

    /**
     * A few lines about the crash in `record`: the signal, what was said, and the
     * places the crashed thread was in, innermost first, in at most `frames` lines.
     * A long stretch inside one library is shortened to its first two places and its
     * last one (the inside of the memory allocator says little, the way into it much).
     */
    static List<String> summary(byte[] record, int frames) throws IOException {
        Reader all = new Reader(record, 0, record.length);
        long crashed = 0;
        String signal = null;
        String said = null;
        Map<Long, Reader> threads = new HashMap<>();
        while (all.more()) {
            int field = all.field();
            if (field == 6 && all.kind == 0) { // tid
                crashed = all.number();
            } else if (field == 10 && all.kind == 2) { // signal_info
                signal = signal(all.stretch());
            } else if (field == 14 && all.kind == 2) { // abort_message
                said = all.text();
            } else if (field == 16 && all.kind == 2) { // threads: id -> thread
                Reader entry = all.stretch();
                long id = 0;
                Reader thread = null;
                while (entry.more()) {
                    int part = entry.field();
                    if (part == 1 && entry.kind == 0) {
                        id = entry.number();
                    } else if (part == 2 && entry.kind == 2) {
                        thread = entry.stretch();
                    } else {
                        entry.skip();
                    }
                }
                if (thread != null) {
                    threads.put(id, thread);
                }
            } else {
                all.skip();
            }
        }

        List<String> lines = new ArrayList<>();
        Reader thread = threads.get(crashed);
        String name = null;
        List<String> places = new ArrayList<>();
        List<String> libraries = new ArrayList<>();
        String build = null;
        while (thread != null && thread.more()) {
            int field = thread.field();
            if (field == 2 && thread.kind == 2) { // name
                name = thread.text();
            } else if (field == 4 && thread.kind == 2) { // current_backtrace
                Reader frame = thread.stretch();
                long offset = 0;
                long into = 0;
                String function = null;
                String file = null;
                String id = null;
                while (frame.more()) {
                    int part = frame.field();
                    if (part == 1 && frame.kind == 0) { // rel_pc
                        offset = frame.number();
                    } else if (part == 4 && frame.kind == 2) { // function_name
                        function = frame.text();
                    } else if (part == 5 && frame.kind == 0) { // function_offset
                        into = frame.number();
                    } else if (part == 6 && frame.kind == 2) { // file_name
                        file = frame.text();
                    } else if (part == 8 && frame.kind == 2) { // build_id
                        id = frame.text();
                    } else {
                        frame.skip();
                    }
                }
                String library = file == null ? "?" : file.substring(file.lastIndexOf('/') + 1);
                if (build == null && library.equals("liboneum.so") && id != null) {
                    build = id;
                }
                String place = String.format(Locale.ROOT, "#%02d %s+0x%x", places.size(), library, offset);
                if (function != null && !function.isEmpty()) {
                    place += " (" + function + "+" + into + ")";
                }
                places.add(shortened(place));
                libraries.add(library);
            } else {
                thread.skip();
            }
        }
        String head = signal == null ? "no signal told" : signal;
        if (name != null && !name.isEmpty()) {
            head += ", on thread \"" + name + "\"";
        }
        lines.add(head);
        if (said != null && !said.isEmpty()) {
            lines.add(shortened("said: " + said.replace('\n', ' ')));
        }
        int shown = 0;
        for (int from = 0; from < places.size() && shown < frames; ) {
            int to = from;
            while (to < places.size() && libraries.get(to).equals(libraries.get(from))) {
                to += 1;
            }
            if (to - from <= 3) {
                for (int i = from; i < to; i++, shown++) {
                    lines.add(places.get(i));
                }
            } else {
                lines.add(places.get(from));
                lines.add(places.get(from + 1));
                lines.add("    … " + (to - from - 3) + " more in " + libraries.get(from));
                lines.add(places.get(to - 1));
                shown += 4;
            }
            from = to;
        }
        if (build != null) {
            lines.add("liboneum.so is build " + build);
        }
        return lines;
    }

    private static String signal(Reader signal) throws IOException {
        String name = null;
        String code = null;
        boolean hasAddress = false;
        long address = 0;
        while (signal.more()) {
            int field = signal.field();
            if (field == 2 && signal.kind == 2) {
                name = signal.text();
            } else if (field == 4 && signal.kind == 2) {
                code = signal.text();
            } else if (field == 8 && signal.kind == 0) {
                hasAddress = signal.number() != 0;
            } else if (field == 9 && signal.kind == 0) {
                address = signal.number();
            } else {
                signal.skip();
            }
        }
        String text = name == null ? "a signal" : name;
        if (code != null && !code.isEmpty()) {
            text += " (" + code + ")";
        }
        if (hasAddress) {
            text += String.format(" at 0x%x", address);
        }
        return text;
    }

    private static String shortened(String line) {
        return line.length() <= 150 ? line : line.substring(0, 150) + "…";
    }
}
