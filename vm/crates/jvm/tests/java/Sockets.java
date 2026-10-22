import java.net.*;
import java.nio.ByteBuffer;
import java.nio.channels.*;
import java.nio.charset.StandardCharsets;
import java.util.Iterator;

/** NIO sockets and selectors over loopback (what Netty's NIO transport uses). */
public class Sockets {
    public static void main(String[] args) throws Exception {
        try (Selector selector = Selector.open();
             ServerSocketChannel server = ServerSocketChannel.open()) {
            server.bind(new InetSocketAddress(InetAddress.getLoopbackAddress(), 0));
            server.configureBlocking(false);
            server.register(selector, SelectionKey.OP_ACCEPT);
            int port = ((InetSocketAddress) server.getLocalAddress()).getPort();
            System.out.println("listening: " + (port > 0));

            Thread client = new Thread(() -> {
                try (SocketChannel channel = SocketChannel.open(new InetSocketAddress("localhost", port))) {
                    channel.write(ByteBuffer.wrap("hello over loopback".getBytes(StandardCharsets.UTF_8)));
                    ByteBuffer reply = ByteBuffer.allocate(64);
                    while (reply.position() < 4) channel.read(reply);
                    reply.flip();
                    System.out.println("client got: " + StandardCharsets.UTF_8.decode(reply));
                } catch (Exception e) {
                    e.printStackTrace();
                }
            });
            client.start();

            String received = null;
            while (received == null) {
                selector.select(5000);
                Iterator<SelectionKey> keys = selector.selectedKeys().iterator();
                while (keys.hasNext()) {
                    SelectionKey key = keys.next();
                    keys.remove();
                    if (key.isAcceptable()) {
                        SocketChannel accepted = server.accept();
                        accepted.configureBlocking(false);
                        accepted.register(selector, SelectionKey.OP_READ);
                        System.out.println("accepted from loopback: " + ((InetSocketAddress) accepted.getRemoteAddress()).getAddress().isLoopbackAddress());
                    } else if (key.isReadable()) {
                        SocketChannel channel = (SocketChannel) key.channel();
                        ByteBuffer buffer = ByteBuffer.allocate(64);
                        int n = channel.read(buffer);
                        if (n > 0) {
                            buffer.flip();
                            received = StandardCharsets.UTF_8.decode(buffer).toString();
                            channel.write(ByteBuffer.wrap("pong".getBytes(StandardCharsets.UTF_8)));
                        }
                    }
                }
            }
            client.join(); // then print, for a deterministic order
            System.out.println("server got: " + received);

            // wakeup() must interrupt a blocking select.
            Thread waker = new Thread(() -> {
                try { Thread.sleep(100); } catch (InterruptedException ignored) {}
                selector.wakeup();
            });
            waker.start();
            int selected = selector.select(10_000);
            System.out.println("woken with " + selected + " keys");
            waker.join();
        }
    }
}
