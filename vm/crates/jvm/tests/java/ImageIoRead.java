// javax.imageio: a PNG read into a BufferedImage (AWT runs headless; its
// initIDs natives are no-ops), compared with HotSpot.
import java.awt.image.BufferedImage;
import java.io.ByteArrayInputStream;
import java.util.Base64;
import javax.imageio.ImageIO;
import vmtest.Out;

public class ImageIoRead {
    static final String PNG = "iVBORw0KGgoAAAANSUhEUgAAAAQAAAAECAYAAACp8Z5+AAAANElEQVR4nBXKQREAMBACMYQhjCeiELGurtO8I4laXEQnTjJn05ibqRTqcAld/hjn0Yzb6ANPjCO5wNRw2gAAAABJRU5ErkJggg==";
    public static void main(String[] args) throws Exception {
        BufferedImage image = ImageIO.read(new ByteArrayInputStream(Base64.getDecoder().decode(PNG)));
        Out.println(image.getWidth() + "x" + image.getHeight() + " type " + image.getType());
        StringBuilder s = new StringBuilder();
        for (int y = 0; y < 4; y++) for (int x = 0; x < 4; x++) s.append(Integer.toHexString(image.getRGB(x, y))).append(' ');
        Out.println(s.toString().trim());
        Out.println(image.getColorModel().hasAlpha() + " " + image.getRaster().getNumBands());
    }
}
