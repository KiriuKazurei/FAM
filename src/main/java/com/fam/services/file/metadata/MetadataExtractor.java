package com.fam.services.file.metadata;

import com.fam.services.file.scanner.FileScannerService;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.nio.file.attribute.BasicFileAttributes;
import java.nio.file.attribute.FilesReadAttributes;
import java.util.HashMap;
import java.util.Map;
import javax.imageio.ImageIO;
import javax.imageio.ImageReader;
import javax.imageio.stream.ImageInputStream;
import org.libraw.LibRaw;
import org.libraw.LibRawException;


// TODO
public class MetadataExtractor {

    private final FileScannerService fileScannerService;
    private final LocalInferenceService localInferenceService;

    public MetadataExtractor(FileScannerService fileScannerService, LocalInferenceService localInferenceService) {
        this.fileScannerService = fileScannerService;
        this.localInferenceService = localInferenceService;
    }

    public Map<String, Object> extractMetadata(Path filePath) {
        Map<String, Object> metadata = new HashMap<>();
        try {
            // 基本文件属性
            BasicFileAttributes attrs = Files.readAttributes(filePath, BasicFileAttributes.class);
            metadata.put("fileName", filePath.getFileName().toString());
            metadata.put("fileSize", attrs.size());
            metadata.put("lastModifiedTime", attrs.lastModifiedTime().toMillis());

            // 图像元数据
            if (filePath.toString().endsWith(".raw")) {
                extractRAWMetadata(filePath, metadata);
            } else {
                extractImageMetadata(filePath, metadata);
            }

            // AI标签生成
            if (fileScannerService.isAIAvailable()) {
                String[] aiTags = localInferenceService.generateTags(filePath.toString());
                metadata.put("aiTags", aiTags);
            }

        } catch (IOException | LibRawException e) {
            e.printStackTrace();
        }
        return metadata;
    }

    private void extractImageMetadata(Path filePath, Map<String, Object> metadata) throws IOException {
        try (ImageInputStream iis = ImageIO.createImageInputStream(filePath.toFile())) {
            ImageReader reader = ImageIO.getImageReaders(iis).next();
            reader.setInput(iis, true);
            metadata.put("width", reader.getWidth(0));
            metadata.put("height", reader.getHeight(0));
            metadata.put("format", reader.getFormatName());
        }
    }

    private void extractRAWMetadata(Path filePath, Map<String, Object> metadata) throws LibRawException {
        LibRaw rawProcessor = new LibRaw();
        rawProcessor.open_file(filePath.toString());
        rawProcessor.unpack();
        metadata.put("width", rawProcessor.imgdata.idata.width);
        metadata.put("height", rawProcessor.imgdata.idata.height);
        metadata.put("make", rawProcessor.imgdata.idata.make);
        metadata.put("model", rawProcessor.imgdata.idata.model);
        metadata.put("iso", rawProcessor.imgdata.other.iso_speed);
        metadata.put("exposureTime", rawProcessor.imgdata.other.shutter);
        metadata.put("aperture", rawProcessor.imgdata.other.aperture);
        metadata.put("focalLength", rawProcessor.imgdata.other.focal_len);
    }

    public static void main(String[] args) {
        FileScannerService fileScannerService = new FileScannerService();
        LocalInferenceService localInferenceService = new LocalInferenceService();
        MetadataExtractor metadataExtractor = new MetadataExtractor(fileScannerService, localInferenceService);

        Path filePath = Paths.get("D:\\Datas\\Programs\\PGFile\\FileAutoManage(FAM)\\data\\example.jpg");
        Map<String, Object> metadata = metadataExtractor.extractMetadata(filePath);
        System.out.println(metadata);
    }
}