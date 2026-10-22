import java.io.*;
import java.nio.ByteBuffer;
import java.nio.channels.*;
import java.nio.charset.StandardCharsets;
import java.nio.file.*;
import java.nio.file.attribute.BasicFileAttributes;
import java.util.*;
import java.util.stream.*;
import java.util.zip.*;

/** File APIs Minecraft relies on: NIO Files, channels, locks, stores, zip/gzip. */
public class FilesApi {
    public static void main(String[] args) throws Exception {
        Path root = Paths.get("target", "files-api-test").toAbsolutePath();
        if (Files.exists(root)) {
            try (Stream<Path> walk = Files.walk(root)) {
                walk.sorted(Comparator.reverseOrder()).forEach(p -> p.toFile().delete());
            }
        }
        Files.createDirectories(root.resolve("a/b/c"));
        Path file = root.resolve("a/b/c/data.txt");
        Files.writeString(file, "line one\nline two\n");
        System.out.println("read: " + Files.readAllLines(file));
        System.out.println("size: " + Files.size(file) + ", exists: " + Files.exists(file) + ", dir: " + Files.isDirectory(file.getParent()));

        Path tmp = root.resolve("a/level.dat_tmp");
        Files.write(tmp, new byte[] {1, 2, 3, 4});
        Path moved = root.resolve("a/level.dat");
        Files.move(tmp, moved, StandardCopyOption.ATOMIC_MOVE, StandardCopyOption.REPLACE_EXISTING);
        Files.copy(moved, root.resolve("a/level.dat_old"), StandardCopyOption.REPLACE_EXISTING);
        System.out.println("moved: " + Files.exists(moved) + " " + !Files.exists(tmp) + " copy=" + Arrays.toString(Files.readAllBytes(root.resolve("a/level.dat_old"))));

        try (Stream<Path> walk = Files.walk(root)) {
            System.out.println("walk: " + walk.map(p -> root.relativize(p).toString()).sorted().collect(Collectors.toList()));
        }
        try (DirectoryStream<Path> dir = Files.newDirectoryStream(root.resolve("a"))) {
            List<String> names = new ArrayList<>();
            for (Path p : dir) names.add(p.getFileName().toString());
            Collections.sort(names);
            System.out.println("list: " + names);
        }
        BasicFileAttributes attrs = Files.readAttributes(file, BasicFileAttributes.class);
        System.out.println("attrs: regular=" + attrs.isRegularFile() + " size=" + attrs.size());

        FileStore store = Files.getFileStore(root);
        System.out.println("store usable > 0: " + (store.getUsableSpace() > 0));

        try (FileChannel channel = FileChannel.open(root.resolve("session.lock"), StandardOpenOption.CREATE, StandardOpenOption.WRITE)) {
            channel.write(ByteBuffer.wrap("☃".getBytes(StandardCharsets.UTF_8)));
            FileLock lock = channel.tryLock();
            System.out.println("locked: " + (lock != null && lock.isValid()));
            lock.release();
        }
        try (RandomAccessFile raf = new RandomAccessFile(root.resolve("region.mca").toFile(), "rw")) {
            raf.setLength(8192);
            raf.seek(4096);
            raf.writeInt(0xCAFEBABE);
            raf.seek(4096);
            System.out.println("raf: " + Integer.toHexString(raf.readInt()) + " length=" + raf.length());
        }

        Path gz = root.resolve("level.nbt.gz");
        try (OutputStream out = new GZIPOutputStream(Files.newOutputStream(gz))) {
            for (int i = 0; i < 1000; i++) out.write(("entry " + i + "\n").getBytes(StandardCharsets.UTF_8));
        }
        try (BufferedReader in = new BufferedReader(new InputStreamReader(new GZIPInputStream(Files.newInputStream(gz))))) {
            System.out.println("gzip lines: " + in.lines().count() + ", compressed smaller: " + (Files.size(gz) < 8890));
        }
        Deflater deflater = new Deflater(Deflater.BEST_SPEED);
        byte[] input = "chunk data ".repeat(200).getBytes(StandardCharsets.UTF_8);
        deflater.setInput(input);
        deflater.finish();
        byte[] compressed = new byte[4096];
        int clen = deflater.deflate(compressed);
        Inflater inflater = new Inflater();
        inflater.setInput(compressed, 0, clen);
        byte[] output = new byte[input.length];
        int olen = inflater.inflate(output);
        System.out.println("zlib roundtrip: " + (olen == input.length && Arrays.equals(input, output)));

        Files.delete(file);
        System.out.println("deleted: " + !Files.exists(file));
        try {
            Files.delete(root.resolve("a"));
        } catch (DirectoryNotEmptyException e) {
            System.out.println("DirectoryNotEmptyException");
        }
        try {
            Files.readString(root.resolve("missing.txt"));
        } catch (NoSuchFileException e) {
            System.out.println("NoSuchFileException");
        }
    }
}
