package com.fam.services.file.metadata;

/**
 * @author Kazurei
 * @Description
 * @date 2025/3/11 0:47
 * @Project FileAutoManage(FAM)
 **/

import org.springframework.boot.web.servlet.server.Session;
import org.tensorflow.*;

import javax.imageio.ImageIO;
import java.awt.image.BufferedImage;
import java.io.File;
import java.util.Arrays;
import java.util.List;

public class LocalInferenceService {
    private final boolean aiEnabled;
    private final SavedModelBundle model;
    private static final String MODEL_PATH = "models/mobilenetv3_int8.onnx"; // 模型路径

    public LocalInferenceService() {
        this.aiEnabled = HardwareDetector.isHardwareSupported();
        if (aiEnabled) {
            // 初始化模型（实际开发需考虑线程安全和资源管理）
            model = SavedModelBundle.load(MODEL_PATH, "serve");
        }
    }

    public String[] generateTags(String filePath) {
        if (!aiEnabled) return new String[0]; // 硬件不足时返回空

        try {
            // 1. 读取图片文件
            File imageFile = new File(filePath);
            BufferedImage image = ImageIO.read(imageFile);

            // 2. 预处理（假设存在预处理工具类）
            float[][][] inputTensor = ImagePreprocessor.resizeAndNormalize(image, 224, 224);

            // 3. 构建Tensor
            Tensor<Float> input = Tensor.create(inputTensor, Float.class);

            // 4. 执行推理
            Session session = model.session();
            Tensor<Float> output = session.runner()
                    .feed("input", input)
                    .fetch("output")
                    .run().get(0).expect(Float.class);

            // 5. 处理输出（假设模型输出是概率最高的5个标签索引）
            float[] probabilities = output.copyTo(new float[output.shape()[0]]);
            int[] topIndices = getTopKIndices(probabilities, 5);

            // 6. 转换为标签名称（需要标签映射表）
            List<String> labels = LabelLoader.loadLabels("imagenet_labels.txt");
            String[] result = Arrays.stream(topIndices)
                    .mapToObj(i -> labels.get(i))
                    .toArray(String[]::new);

            return result;
        } catch (Exception e) {
            e.printStackTrace();
            return new String[0];
        }
    }

    // 辅助方法：获取Top-K索引
    private static int[] getTopKIndices(float[] array, int k) {
        // 实现逻辑：返回前k大的元素索引（需自行实现或使用第三方库）
        return new int[0]; // 占位
    }
}

// 需要补充以下辅助类（未在当前代码中体现）
class HardwareDetector {
   static boolean isHardwareSupported() { /* 检测逻辑 */ }
}

class ImagePreprocessor {
   static float[][][] resizeAndNormalize(BufferedImage image, int width, int height) { /* 预处理逻辑 */ }
}

class LabelLoader {
   static List<String> loadLabels(String path) { /* 加载标签映射文件 */
       return null;
       // TODO
   }
}



