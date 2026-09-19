// 差别分析，其实直接用哈希码对比就可以了，这里抽象成接口因为打算重写成两个方向
// 方向一：直接实现这个类，但是这样会让java拖累枕骨性能
// 方向二（计划使用）：通过override的方式，实现调用go的哈希计算分析
// 这里要么直接接受一个布尔值要么就是接收利用go运算出来的哈希码，再用java进行哈希码对比就可以了


public interface DiffAnalyzer {

    /**
     * 分析两个文件的差异。
     * @param file1Path 第一个文件路径
     * @param file2Path 第二个文件路径
     * @return 是否存在差异
     */
    boolean analyzeDifference(String file1Path, String file2Path);

    /**
     * 比较两个哈希值。
     * @param hash1 哈希值1
     * @param hash2 哈希值2
     * @return 是否相等
     */
    default boolean compareHashes(String hash1, String hash2) {
        return hash1.equals(hash2);
    }
}
