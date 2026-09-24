#ifndef PREVIEW_MODEL_H
#define PREVIEW_MODEL_H

#include <QAbstractListModel>
#include <QDateTime>

/* One level of a real directory, as a list model.
 *
 * This exists so the design can be looked at with real content: names, sizes and dates a person can
 * recognise, instead of a table of "File One / 1.2 MB". It reads the folder once, on the calling
 * thread, and never watches it - watching is the product's job, and this app is not the product.
 *
 * The role names match `FamListModel` in `apps/fam-shell` on purpose. A design sample that speaks a
 * different vocabulary from the real model is a design sample that will need translating twice. */
class PreviewModel final : public QAbstractListModel {
    Q_OBJECT

public:
    enum Roles {
        NameRole = Qt::UserRole + 1,
        KindRole,
        SizeRole,
        ModifiedRole,
        PathRole,
        DirectoryRole,
    };

    explicit PreviewModel(QObject *parent = nullptr);

    /* Lists `directory`, folders first, each group ordered by `sortKey` ("name", "size", "time").
     *
     * `needle` and `kind` filter, because the design has to be looked at in its filtered and empty
     * states - those are the states where a list usually goes wrong. `kind` of -1 is every bucket.
     *
     * Returns the row count. An unreadable directory yields an empty model rather than an exception,
     * and `isReadable()` says which of the two an empty result was, because "nothing matched" and "I
     * could not open this" are opposite sentences. */
    int load(const QString &directory, const QString &needle = QString(), int kind = -1,
             const QString &sortKey = QStringLiteral("name"));

    QString directory() const { return m_directory; }

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    QHash<int, QByteArray> roleNames() const override;

    /* The bucket of one row, for a caller that counts them. */
    int kindAt(int row) const { return m_rows.at(row).kind; }
    bool isReadable() const { return m_readable; }
    QString pathAt(int row) const { return m_rows.at(row).path; }
    bool isDirectory(int row) const { return m_rows.at(row).directory; }

private:
    struct Entry {
        QString name;
        QString path;
        quint64 size = 0;
        qint64 modifiedMs = 0;
        bool directory = false;
        int kind = 0;
    };

    QString m_directory;
    QVector<Entry> m_rows;
    bool m_readable = false;
};

/* The kind vocabulary the core uses, so a tag in the design sample says the same word as the product
 * would. `kindFor` is the same mapping the schedule applies to a `FamViewRow`'s kind byte. */
namespace preview {

QString kindName(int kind);
int kindFor(bool directory, const QString &name);
/* A byte count the way a file list says it: one decimal, binary units. */
QString formatSize(quint64 bytes);
/* A modification time short enough to sit in a row: "3 天前" style relative dates are a different
 * design decision, so this shows the calendar date and the clock only for today. */
QString formatWhen(qint64 epochMs);

} /* namespace preview */

#endif /* PREVIEW_MODEL_H */
