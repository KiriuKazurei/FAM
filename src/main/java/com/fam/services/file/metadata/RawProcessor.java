package com.fam.services.file.metadata;

import org.libraw.LibRaw;
import org.libraw.LibRawException;
import javax.imageio.ImageIO;
import java.awt.image.BufferedImage;
import java.io.File;
import java.io.IOException;

// TODO 等待修复
public class RawProcessor {

    private LibRaw rawProcessor;

    public RawProcessor() {
        this.rawProcessor = new LibRaw();
    }

    public BufferedImage loadRAW(String filePath) throws LibRawException {
        rawProcessor.open_file(filePath);
        rawProcessor.unpack();
        rawProcessor.raw2image();
        return rawProcessor.get_image_as_buffered_image();
    }

    public BufferedImage generateThumbnail(String filePath, int maxDimension) throws LibRawException, IOException {
        rawProcessor.open_file(filePath);
        rawProcessor.unpack_thumb();
        BufferedImage thumbnail = rawProcessor.get_thumb_as_buffered_image();
        return resizeImage(thumbnail, maxDimension);
    }

    private BufferedImage resizeImage(BufferedImage originalImage, int maxDimension) {
        int originalWidth = originalImage.getWidth();
        int originalHeight = originalImage.getHeight();
        double aspectRatio = (double) originalWidth / originalHeight;
        int newWidth, newHeight;

        if (originalWidth > originalHeight) {
            newWidth = maxDimension;
            newHeight = (int) (newWidth / aspectRatio);
        } else {
            newHeight = maxDimension;
            newWidth = (int) (newHeight * aspectRatio);
        }

        BufferedImage resizedImage = new BufferedImage(newWidth, newHeight, BufferedImage.TYPE_INT_RGB);
        resizedImage.getGraphics().drawImage(originalImage, 0, 0, newWidth, newHeight, null);
        return resizedImage;
    }

    public static void main(String[] args) {
        RawProcessor rawProcessor = new RawProcessor();
        try {
            String filePath = "D:\\Datas\\Programs\\PGFile\\FileAutoManage(FAM)\\data\\example.RAF";
            BufferedImage thumbnail = rawProcessor.generateThumbnail(filePath, 256);
            ImageIO.write(thumbnail, "jpg", new File("thumbnail.jpg"));
            System.out.println("Thumbnail generated successfully.");
        } catch (LibRawException | IOException e) {
            e.printStackTrace();
        }
    }
}