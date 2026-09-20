#ifndef FAMLISTMODEL_H
#define FAMLISTMODEL_H

#include <QAbstractListModel>
#include <QJsonArray>
#include <QJsonObject>
#include <QVector>

struct FamRow {
    QString key;
    QString path;
    QString name;
    QString kind;
    QString tags;
    qint64 size = 0;
    qint64 modifiedMs = 0;
};

/* List model fed by the core's positioned deltas.
 *
 * A search replaces the whole projection through a model reset; everything after that arrives as
 * insert / change / remove with an index, so a batch of a hundred file events costs one set of
 * row signals rather than a hundred resets.
 */
class FamListModel final : public QAbstractListModel {
    Q_OBJECT

public:
    enum Roles {
        KeyRole = Qt::UserRole + 1,
        PathRole,
        NameRole,
        KindRole,
        SizeRole,
        ModifiedRole,
        TagsRole,
    };

    explicit FamListModel(QObject *parent = nullptr);

    int rowCount(const QModelIndex &parent = QModelIndex()) const override;
    QVariant data(const QModelIndex &index, int role) const override;
    QHash<int, QByteArray> roleNames() const override;

    void load(const QJsonArray &rows);
    void applyDeltas(const QJsonArray &deltas);
    void clearRows();

    QString pathAt(int index) const;
    int count() const { return m_rows.size(); }

private:
    static FamRow rowFromJson(const QJsonObject &object);

    QVector<FamRow> m_rows;
};

QString formatSize(qint64 bytes);

#endif /* FAMLISTMODEL_H */
