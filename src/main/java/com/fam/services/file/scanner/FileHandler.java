package com.fam.services.file.scanner;

import com.fam.services.file.ai.AIManager;
import com.fam.services.file.ai.AIModule;
import com.fam.services.file.metadata.MetadataExtractor;
import com.fam.services.file.metadata.ExifParser;
import com.fam.services.file.metadata.RawProcessor;
import com.fam.services.file.version.ContentAddressableStore;
import com.fam.services.file.version.FileVersion;
import org.slf4j.Logger;
import org.slf4j.LoggerFactory;

import java.awt.image.BufferedImage;
import java.io.File;
import java.io.IOException;
import java.nio.file.Files;
import java.nio.file.Path;
import java.nio.file.Paths;
import java.util.List;

/**
 * @author Kazurei
 * @Description 文件处理器，负责图片的读取、缩略图生成、AI标签生成等
 * @date 2025/3/11 0:50
 * @Project FileAutoManage(FAM)
 **/
public class FileHandler {
    private static final Logger logger = LoggerFactory.getLogger(FileHandler.class);
    private final MetadataExtractor metadataExtractor;
    private final RawProcessor rawProcessor;
    private final ExifParser exifParser;
    private final ContentAddressableStore casStore;
    private final AIModule aiModule;

    public FileHandler(MetadataExtractor metadataExtractor, RawProcessor rawProcessor, ExifParser exifParser, ContentAddressableStore casStore, AIModule aiModule) {
        this.metadataExtractor = metadataExtractor;
        this.rawProcessor = rawProcessor;
        this.exifParser = exifParser;
        this.casStore = casStore;
        this.aiModule = aiModule;
    }

    public void processFile(String filePath) {
        try {
            File file = new File(filePath);
            if (!file.exists()) {
                logger.error("文件不存在: {}", filePath);
                return;
            }

            // 读取文件元数据
            String metadata = metadataExtractor.extractMetadata(file);
            logger.info("提取元数据: {}", metadata);

            // 处理RAW文件
            if (filePath.toLowerCase().endsWith(".raw")) {
                BufferedImage thumbnail = rawProcessor.generateThumbnail(file);
                logger.info("生成RAW缩略图: {}", thumbnail);

                // 保存缩略图
                Path thumbnailPath = Paths.get(file.getParent(), "thumbnails", file.getName() + ".thumbnail");
                Files.createDirectories(thumbnailPath.getParent());
                ImageIO.write(thumbnail, "png", thumbnailPath.toFile());
            }

            // 解析EXIF数据
            String exifData = exifParser.parseExif(file);
            logger.info("解析EXIF数据: {}", exifData);

            // 生成AI标签
            if (aiModule.isAvailable()) {
                List<String> aiTags = aiModule.generateTags(file);
                logger.info("生成AI标签: {}", aiTags);
            }

            // 创建文件版本快照
            FileVersion fileVersion = casStore.createSnapshot(file.toPath());
            logger.info("创建文件版本快照: {}", fileVersion);

        } catch (IOException e) {
            logger.error("处理文件时发生错误: {}", filePath, e);
        }
    }
}