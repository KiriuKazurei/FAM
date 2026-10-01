#include "renamedialog.h"

#include <QDir>
#include <QFileInfo>
#include <QHBoxLayout>
#include <QJsonObject>
#include <QLabel>
#include <QLineEdit>
#include <QPushButton>
#include <QVBoxLayout>

#include "bridge.h"
#include "theme.h"

RenameDialog::RenameDialog(FamBridge *bridge, const QString &path, QWidget *parent)
    : QDialog(parent), m_bridge(bridge), m_path(path), m_from(path) {
    setWindowTitle(QStringLiteral("Rename — %1").arg(QFileInfo(path).fileName()));
    /* Named like every other surface: an unnamed QDialog takes the desktop's scheme, and the wire
     * assertions would pass green anyway (the tag dialog's first screenshot taught this). */
    setObjectName(QString::fromLatin1(Theme::pageName()));

    auto *heading = new QLabel(QDir::toNativeSeparators(path), this);
    heading->setToolTip(path);

    m_entry = new QLineEdit(this);
    m_entry->setObjectName(QString::fromLatin1(Theme::noteEditorName()));
    m_entry->setText(QFileInfo(path).fileName());
    m_entry->selectAll();

    m_line = new QLabel(this);
    m_line->setWordWrap(true);

    auto *okButton = new QPushButton(QStringLiteral("Rename"), this);
    okButton->setObjectName(QString::fromLatin1(Theme::primaryButtonName()));
    auto *cancelButton = new QPushButton(QStringLiteral("Cancel"), this);
    connect(cancelButton, &QPushButton::clicked, this, &QDialog::reject);

    auto *buttons = new QHBoxLayout;
    buttons->addWidget(m_line, 1);
    buttons->addWidget(okButton);
    buttons->addWidget(cancelButton);

    auto *layout = new QVBoxLayout(this);
    layout->addWidget(heading);
    layout->addWidget(m_entry);
    layout->addLayout(buttons);

    connect(okButton, &QPushButton::clicked, this, [this] {
        QString error;
        if (!applyName(&error)) {
            m_line->setText(error);
            return;
        }
        accept();
    });
}

QString RenameDialog::nameText() const {
    return m_entry != nullptr ? m_entry->text() : QString();
}

void RenameDialog::setNameText(const QString &text) {
    if (m_entry != nullptr) {
        m_entry->setText(text);
    }
}

QString RenameDialog::noteLine() const {
    return m_line != nullptr ? m_line->text() : QString();
}

bool RenameDialog::applyName(QString *error) {
    const QString name = nameText().trimmed();
    if (name.isEmpty()) {
        *error = QStringLiteral("a name needs some text");
        return false;
    }
    if (name == QFileInfo(m_path).fileName()) {
        *error = QStringLiteral("that is the name it already has");
        return false;
    }
    const QString destination =
        QDir::cleanPath(QFileInfo(m_path).dir().filePath(name));
    QJsonValue result;
    if (!m_bridge->pathOp(m_path, destination, false, &result, error)) {
        return false;
    }
    m_to = result.toObject().value(QStringLiteral("to")).toString();
    m_changed = true;
    return true;
}
